use super::*;
use serde_json::json;

fn redirect() -> Value {
    json!({
        "protocol":"all", "pref":49152, "kind":"matchall", "chain":0,
        "options":{"handle":48336,"actions":[{
            "kind":"mirred", "to_dev":"bc-limit0", "mirred_action":"redirect",
            "direction":"egress", "control_action":{"type":"stolen"}
        }]}
    })
}

#[test]
fn traffic_limit_redirect_verification_requires_full_ingress_coverage() {
    let valid = redirect();
    assert!(owned_filter(&valid));
    assert!(exclusive_ingress(std::slice::from_ref(&valid)));
    for (pointer, value) in [
        ("/protocol", json!("ip")),
        ("/protocol", json!("ipv6")),
        ("/chain", json!(1)),
        ("/chain", json!("0")),
        ("/options/skip_sw", json!(true)),
        ("/options/actions/0/control_action/type", json!("pass")),
        ("/options/actions/0/control_action/type", json!("pipe")),
        ("/options/actions/0/to_dev", json!("other0")),
    ] {
        let mut changed = valid.clone();
        if pointer == "/options/skip_sw" {
            changed["options"]["skip_sw"] = value;
        } else {
            *changed.pointer_mut(pointer).unwrap() = value;
        }
        assert!(!owned_filter(&changed), "{pointer}: {changed}");
        assert!(!exclusive_ingress(&[valid.clone(), changed]));
    }
    let mut shared = valid.clone();
    shared["block"] = 123.into();
    assert!(!owned_filter(&shared));
    assert!(!exclusive_ingress(&[shared]));
    let header = json!({"protocol":"all","pref":49152,"kind":"matchall","chain":0});
    assert!(exclusive_ingress(&[header.clone(), valid.clone()]));
    for (key, value) in [("protocol", json!("ip")), ("chain", json!(1))] {
        let mut changed = header.clone();
        changed[key] = value;
        assert!(!exclusive_ingress(&[changed, valid.clone()]));
    }
}

struct SharedIngress;
impl Network for SharedIngress {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
        match (program, args) {
            ("tc", ["-j", "qdisc", "show", "dev", "eth0"]) => Ok(json!([
                {"kind":"noqueue","handle":"0:","root":true},
                {"kind":"clsact","handle":"ffff:","parent":"ffff:fff1","ingress_block":123}
            ])
            .to_string()),
            ("ip", ["-j", "-d", "link", "show"]) => Ok(json!([
                {"ifname":"eth0"},
                {"ifname":IFB,"ifalias":OWNER,"linkinfo":{"info_kind":"ifb"}}
            ])
            .to_string()),
            _ => panic!(
                "shared ingress must be rejected before further commands: {program} {args:?}"
            ),
        }
    }
}

#[test]
fn traffic_limit_shared_ingress_is_rejected_before_checkpoint_or_mutation() {
    let dir = std::env::temp_dir().join(format!("brocade-limit-shared-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let error =
        reconcile_network(&dir, Some("eth0"), Some(30_000_000), &mut SharedIngress).unwrap_err();
    assert!(error.contains("共享入站"), "{error}");
    assert!(!dir.join(NETWORK).exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn traffic_limit_cleanup_preserves_shared_ingress_and_ifb() {
    let error = cleanup(
        &AppliedNetwork {
            interface: "eth0".into(),
            previous_root: "fq".into(),
            rate_bps: 30_000_000,
        },
        &mut SharedIngress,
    )
    .unwrap_err();
    assert!(error.contains("共享入站"), "{error}");
}

#[test]
fn traffic_limit_rejects_invalid_network_checkpoints_before_commands() {
    struct NoCommands;
    impl Network for NoCommands {
        fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
            panic!("invalid checkpoint must not touch the network: {program} {args:?}");
        }
    }
    let dir = std::env::temp_dir().join(format!("brocade-limit-checkpoint-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for (interface, previous_root, rate_bps) in [
        ("eth0", "htb", 30_000_000),
        ("../eth0", "fq", 30_000_000),
        (IFB, "fq", 30_000_000),
        ("eth0", "fq", 0),
        ("eth0", "fq", u64::MAX),
    ] {
        let saved = AppliedNetwork {
            interface: interface.into(),
            previous_root: previous_root.into(),
            rate_bps,
        };
        save(&dir.join(NETWORK), &saved).unwrap();
        let before = fs::read(dir.join(NETWORK)).unwrap();
        for rate in [None, Some(30_000_000)] {
            assert!(reconcile_network(&dir, Some("eth0"), rate, &mut NoCommands)
                .unwrap_err()
                .contains("所有权检查点"));
        }
        assert!(cleanup(&saved, &mut NoCommands)
            .unwrap_err()
            .contains("所有权检查点"));
        assert_eq!(before, fs::read(dir.join(NETWORK)).unwrap());
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn traffic_limit_checks_existing_ifb_policy_before_mutating_physical_nic() {
    struct ExistingIfb {
        kind: &'static str,
        filters: Result<&'static str, &'static str>,
    }
    impl Network for ExistingIfb {
        fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
            match (program, args) {
                ("tc", ["-j", "qdisc", "show", "dev", "eth0"]) => {
                    Ok(json!([{"kind":"noqueue","handle":"0:","root":true}]).to_string())
                }
                ("tc", ["-j", "qdisc", "show", "dev", IFB]) => {
                    Ok(json!([{"kind":self.kind,"handle":"1234:","root":true}]).to_string())
                }
                ("tc", ["-j", "filter", "show", "dev", "eth0", "parent", "0:"]) => Ok("[]".into()),
                ("tc", ["-j", "filter", "show", "dev", IFB, "parent", "1234:"]) => {
                    self.filters.map(str::to_owned).map_err(str::to_owned)
                }
                ("ip", ["-j", "-d", "link", "show"]) => Ok(json!([
                    {"ifname":"eth0"},
                    {"ifname":IFB,"ifalias":OWNER,"linkinfo":{"info_kind":"ifb"}}
                ])
                .to_string()),
                _ => panic!("IFB conflict must precede network mutation: {program} {args:?}"),
            }
        }
    }
    let dir =
        std::env::temp_dir().join(format!("brocade-limit-ifb-conflict-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for (kind, filters, expected) in [
        ("tbf", Ok("[]"), "自定义或无法安全接管"),
        ("sfq", Ok(r#"[{"kind":"matchall"}]"#), "自定义过滤"),
        (
            "sfq",
            Err("IFB filter read failed"),
            "IFB filter read failed",
        ),
    ] {
        let error = reconcile_network(
            &dir,
            Some("eth0"),
            Some(30_000_000),
            &mut ExistingIfb { kind, filters },
        )
        .unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert!(!dir.join(NETWORK).exists());
    }
    fs::remove_dir_all(dir).unwrap();
}

pub(super) struct DockerNetwork(String);

impl DockerNetwork {
    pub(super) fn start(tun: bool) -> Self {
        let image =
            std::env::var("BROCADE_TRAFFIC_LIMIT_NET_IMAGE").expect("explicit network test image");
        let executable = std::env::current_exe().unwrap();
        let mount = format!("{}:/brocade-test:ro", executable.display());
        let mut args = vec![
            "run",
            "-d",
            "--volume",
            &mount,
            "--network",
            "none",
            "--cap-add",
            "NET_ADMIN",
        ];
        if tun {
            args.extend(["--device", "/dev/net/tun"]);
        }
        args.extend([&image, "sleep", "180"]);
        let id = crate::run_command("docker", &args).unwrap();
        let mut network = Self(id.trim().to_owned());
        println!("QOS_CONTAINER={}", network.0);
        println!("QOS_TC={}", network.run("tc", &["-V"]).unwrap().trim());
        network
    }
}

impl Network for DockerNetwork {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
        let mut command = vec!["exec", self.0.as_str(), program];
        command.extend_from_slice(args);
        crate::command::run_command_with_timeout("docker", &command, Duration::from_secs(10))
    }

    fn classes(&mut self, interface: &str) -> Result<Vec<kernel::Class>, String> {
        let variable = format!("BROCADE_HTB_INTERFACE={interface}");
        let output = crate::command::run_command_with_timeout(
            "docker",
            &[
                "exec",
                "--env",
                &variable,
                &self.0,
                "/brocade-test",
                "--exact",
                "traffic_limit::tests::traffic_limit_netlink_guest",
                "--ignored",
                "--nocapture",
            ],
            Duration::from_secs(10),
        )?;
        let json = output
            .lines()
            .find_map(|line| line.strip_prefix("HTB_CLASSES="))
            .ok_or("missing kernel class fixture result")?;
        serde_json::from_str(json).map_err(|error| error.to_string())
    }
}

impl Drop for DockerNetwork {
    fn drop(&mut self) {
        let _ = crate::run_command("docker", &["rm", "-f", &self.0]);
    }
}

#[test]
#[ignore = "requires Docker, /dev/net/tun and BROCADE_TRAFFIC_LIMIT_NET_IMAGE with iproute2"]
fn traffic_limit_network_compatibility_with_real_default_mq_and_shared_blocks() {
    let mut network = DockerNetwork::start(true);
    network
        .run(
            "ip",
            &[
                "tuntap",
                "add",
                "dev",
                "limit-tap0",
                "mode",
                "tap",
                "multi_queue",
            ],
        )
        .unwrap();
    network
        .run("ip", &["link", "set", "limit-tap0", "up"])
        .unwrap();
    let before = qdiscs(&mut network, "limit-tap0").unwrap();
    assert!(before
        .iter()
        .any(|q| q["root"] == true && q["kind"] == "mq" && q["handle"] == "0:"));
    assert!(before.iter().any(|q| q["parent"] == ":1"));
    println!("QOS_DEFAULT_MQ={}", serde_json::to_string(&before).unwrap());
    let dir = std::env::temp_dir().join(format!("brocade-limit-compat-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    // Kernel-created mq 0: must survive adoption, repeated rounds, disable and re-adoption.
    for _ in 0..2 {
        for _ in 0..2 {
            reconcile_network(&dir, Some("limit-tap0"), Some(30_000_000), &mut network).unwrap();
            verify_shaper(&mut network, "limit-tap0", 30_000_000).unwrap();
            verify_shaper(&mut network, IFB, 30_000_000).unwrap();
        }
        reconcile_network(&dir, None, None, &mut network).unwrap();
        assert!(qdiscs(&mut network, "limit-tap0")
            .unwrap()
            .iter()
            .any(|q| q["root"] == true && q["kind"] == "mq" && q["handle"] == "0:"));
    }

    // A filter can select HTB's direct path without changing the reported class rate.
    reconcile_network(&dir, Some("limit-tap0"), Some(30_000_000), &mut network).unwrap();
    for interface in ["limit-tap0", IFB] {
        network
            .run(
                "tc",
                &[
                    "filter", "add", "dev", interface, "parent", HANDLE, "protocol", "all", "pref",
                    "1", "matchall", "classid", HANDLE,
                ],
            )
            .unwrap();
        assert!(verify_shaper(&mut network, interface, 30_000_000)
            .unwrap_err()
            .contains("自定义过滤"));
        assert!(
            reconcile_network(&dir, Some("limit-tap0"), Some(50_000_000), &mut network)
                .unwrap_err()
                .contains("自定义过滤")
        );
        // Refusal must precede a rate change on either device.
        assert!(network
            .classes("limit-tap0")
            .unwrap()
            .iter()
            .any(|c| class_matches(c, 30_000_000)));
        assert!(network
            .classes(IFB)
            .unwrap()
            .iter()
            .any(|c| class_matches(c, 30_000_000)));
        network
            .run(
                "tc",
                &[
                    "filter", "del", "dev", interface, "parent", HANDLE, "pref", "1",
                ],
            )
            .unwrap();
    }
    reconcile_network(&dir, None, None, &mut network).unwrap();

    // Legacy ingress is reusable and must remain present after removing our filter.
    network
        .run("tc", &["qdisc", "del", "dev", "limit-tap0", "clsact"])
        .unwrap();
    network
        .run("tc", &["qdisc", "add", "dev", "limit-tap0", "ingress"])
        .unwrap();
    reconcile_network(&dir, Some("limit-tap0"), Some(30_000_000), &mut network).unwrap();
    reconcile_network(&dir, None, None, &mut network).unwrap();
    assert!(qdiscs(&mut network, "limit-tap0")
        .unwrap()
        .iter()
        .any(|q| q["kind"] == "ingress"));

    // Shared egress policy is unrelated to our ingress path and must stay byte-identical.
    network
        .run("tc", &["qdisc", "del", "dev", "limit-tap0", "ingress"])
        .unwrap();
    network
        .run(
            "tc",
            &[
                "qdisc",
                "add",
                "dev",
                "limit-tap0",
                "egress_block",
                "123",
                "clsact",
            ],
        )
        .unwrap();
    network
        .run(
            "tc",
            &[
                "filter", "add", "block", "123", "protocol", "all", "pref", "1", "matchall",
                "action", "pass",
            ],
        )
        .unwrap();
    let egress = network
        .run("tc", &["-j", "filter", "show", "block", "123"])
        .unwrap();
    reconcile_network(&dir, Some("limit-tap0"), Some(30_000_000), &mut network).unwrap();
    reconcile_network(&dir, None, None, &mut network).unwrap();
    assert_eq!(
        egress,
        network
            .run("tc", &["-j", "filter", "show", "block", "123"])
            .unwrap()
    );

    // Both empty and populated shared ingress blocks are refused before any NIC changes.
    network
        .run("tc", &["qdisc", "del", "dev", "limit-tap0", "clsact"])
        .unwrap();
    network
        .run(
            "tc",
            &[
                "qdisc",
                "add",
                "dev",
                "limit-tap0",
                "ingress_block",
                "456",
                "clsact",
            ],
        )
        .unwrap();
    network
        .run("ip", &["link", "add", "peer0", "type", "dummy"])
        .unwrap();
    network
        .run(
            "tc",
            &[
                "qdisc",
                "add",
                "dev",
                "peer0",
                "ingress_block",
                "456",
                "clsact",
            ],
        )
        .unwrap();
    for populated in [false, true] {
        if populated {
            network
                .run(
                    "tc",
                    &[
                        "filter", "add", "block", "456", "protocol", "all", "pref", "1",
                        "matchall", "action", "pass",
                    ],
                )
                .unwrap();
        }
        let before = qdiscs(&mut network, "limit-tap0").unwrap();
        let block = network
            .run("tc", &["-j", "filter", "show", "block", "456"])
            .unwrap();
        assert!(
            reconcile_network(&dir, Some("limit-tap0"), Some(30_000_000), &mut network)
                .unwrap_err()
                .contains("共享入站")
        );
        assert_eq!(before, qdiscs(&mut network, "limit-tap0").unwrap());
        assert_eq!(
            block,
            network
                .run("tc", &["-j", "filter", "show", "block", "456"])
                .unwrap()
        );
        assert!(!dir.join(NETWORK).exists());
    }

    // A same-handle IPv4-only redirect cannot confirm or silently disable a dual-stack cap.
    network
        .run("tc", &["qdisc", "del", "dev", "limit-tap0", "clsact"])
        .unwrap();
    reconcile_network(&dir, Some("limit-tap0"), Some(30_000_000), &mut network).unwrap();
    network
        .run(
            "tc",
            &[
                "filter",
                "del",
                "dev",
                "limit-tap0",
                "ingress",
                "pref",
                "49152",
            ],
        )
        .unwrap();
    network
        .run(
            "tc",
            &[
                "filter",
                "add",
                "dev",
                "limit-tap0",
                "ingress",
                "protocol",
                "ip",
                "pref",
                "49152",
                "handle",
                "0xbcd0",
                "matchall",
                "action",
                "mirred",
                "egress",
                "redirect",
                "dev",
                IFB,
            ],
        )
        .unwrap();
    let before = qdiscs(&mut network, "limit-tap0").unwrap();
    assert!(
        reconcile_network(&dir, Some("limit-tap0"), Some(30_000_000), &mut network)
            .unwrap_err()
            .contains("自定义入站")
    );
    assert!(reconcile_network(&dir, None, None, &mut network)
        .unwrap_err()
        .contains("非自有过滤器"));
    assert_eq!(before, qdiscs(&mut network, "limit-tap0").unwrap());
    assert!(dir.join(NETWORK).exists());
    assert!(json(&mut network, "ip", &["-j", "-d", "link", "show"])
        .unwrap()
        .iter()
        .any(|l| l["ifname"] == IFB));
    network
        .run(
            "tc",
            &[
                "filter",
                "del",
                "dev",
                "limit-tap0",
                "ingress",
                "pref",
                "49152",
            ],
        )
        .unwrap();
    reconcile_network(&dir, None, None, &mut network).unwrap();
    assert!(!dir.join(NETWORK).exists());
    fs::remove_dir_all(dir).unwrap();
}
