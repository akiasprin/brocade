//! Separate plain packet scheduling from shaping/classification policy before taking over a NIC.
use super::{json, Network};
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

pub(super) fn scheduler_kind(kind: &str) -> bool {
    matches!(
        kind,
        "fq" | "fq_codel"
            | "pfifo_fast"
            | "noqueue"
            | "pfifo"
            | "bfifo"
            | "sfq"
            | "codel"
            | "pie"
            | "fq_pie"
    )
}

fn handle(value: &Value) -> Option<(u16, u16)> {
    let (major, minor) = value.as_str()?.split_once(':')?;
    if (major.is_empty() && minor.is_empty())
        || major.len() > 4
        || minor.len() > 4
        || !major
            .bytes()
            .chain(minor.bytes())
            .all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    Some((
        // iproute2 omits the zero major component of mq child parent IDs (:1, :2, ...).
        if major.is_empty() {
            0
        } else {
            u16::from_str_radix(major, 16).ok()?
        },
        if minor.is_empty() {
            0
        } else {
            u16::from_str_radix(minor, 16).ok()?
        },
    ))
}

fn plain_scheduler(queue: &Value) -> bool {
    queue["kind"].as_str().is_some_and(scheduler_kind)
        && matches!(handle(&queue["handle"]), Some((_, 0)))
        // fq is normally a scheduler, but an explicit per-flow rate is still operator QoS.
        && queue["options"].get("maxrate").is_none()
        && queue["options"].get("defrate").is_none()
}

pub(super) fn check_ingress_blocks(queues: &[Value]) -> Result<(), String> {
    // Even an empty shared block cannot be changed through `filter ... dev ... ingress`.
    // Its ownership also extends to other NICs, so never adopt or remove its filters.
    if queues.iter().any(|queue| {
        matches!(queue["kind"].as_str(), Some("clsact" | "ingress"))
            && queue.get("ingress_block").is_some_and(|block| block != 0)
    }) {
        return Err("网卡使用共享入站过滤 block，无法独占接管；保留现有 QoS".into());
    }
    Ok(())
}

pub(super) fn check(queues: &[Value], root: &Value) -> Result<(), String> {
    check_ingress_blocks(queues)?;
    let scheduling: Vec<_> = queues
        .iter()
        .filter(|q| !matches!(q["kind"].as_str(), Some("clsact" | "ingress")))
        .collect();
    if queues.len() > 4096 || scheduling.iter().filter(|q| q["root"] == true).count() != 1 {
        return Err("网卡队列数量超限或根队列不唯一，拒绝覆盖".into());
    }
    let mut handles = BTreeSet::new();
    if !scheduling.iter().all(|q| {
        handle(&q["handle"])
            .is_some_and(|(major, minor)| minor == 0 && (major == 0 || handles.insert(major)))
    }) {
        return Err("网卡队列句柄无效或重复，拒绝覆盖".into());
    }
    let supported = if root["kind"] == "mq" {
        // mq itself only fans out to hardware TX queues. A non-default root handle says
        // nothing about shaping; inspect every child, including children of default mq 0:.
        let mut parents = BTreeSet::new();
        let major = handle(&root["handle"]).filter(|(_, minor)| *minor == 0);
        major.is_some()
            && scheduling.iter().filter(|q| q["root"] != true).all(|q| {
                plain_scheduler(q)
                    && handle(&q["parent"]).is_some_and(|(parent, minor)| {
                        Some((parent, 0)) == major && minor != 0 && parents.insert(minor)
                    })
            })
            && !parents.is_empty()
            && parents.iter().copied().eq(1..=parents.len() as u16)
    } else {
        scheduling.len() == 1 && plain_scheduler(root)
    };
    if supported {
        Ok(())
    } else {
        Err(format!(
            "网卡存在自定义或无法安全接管的 QoS（根队列 {} {}），拒绝覆盖",
            root["kind"].as_str().unwrap_or("未知"),
            root["handle"].as_str().unwrap_or("未知")
        ))
    }
}

pub(super) fn check_filters(
    network: &mut impl Network,
    interface: &str,
    queues: &[Value],
) -> Result<(), String> {
    let mut parents = BTreeSet::new();
    for queue in queues {
        if matches!(queue["kind"].as_str(), Some("clsact" | "ingress")) {
            continue;
        }
        let (major, _) = handle(&queue["handle"]).ok_or("无效网卡队列句柄")?;
        // Unnamed leaves cannot be addressed by a classifier. Named schedulers such as
        // sfq can carry filters even though the scheduler itself has no bandwidth rate.
        if major != 0 || queue["root"] == true {
            parents.insert(format!("{major:x}:"));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    for parent in parents {
        if Instant::now() >= deadline {
            return Err("网卡队列过滤检查超过 3 秒，拒绝覆盖".into());
        }
        if !json(
            network,
            "tc",
            &["-j", "filter", "show", "dev", interface, "parent", &parent],
        )?
        .is_empty()
        {
            return Err(format!("网卡队列 {parent} 已有自定义过滤，拒绝覆盖"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mq() -> Vec<Value> {
        vec![
            json!({"kind":"mq","handle":"1:","root":true,"options":{}}),
            json!({"kind":"fq","handle":"8001:","parent":"1:1","options":{"limit":32768,"flow_limit":32768,"low_rate_threshold":68750}}),
            json!({"kind":"fq","handle":"8002:","parent":"1:2","options":{"limit":32768,"flow_limit":32768,"low_rate_threshold":68750}}),
        ]
    }

    #[test]
    fn traffic_limit_parses_tc_handles_without_accepting_malformed_ids() {
        for (value, expected) in [
            ("0:", (0, 0)),
            (":1", (0, 1)),
            ("0:1", (0, 1)),
            (":a", (0, 10)),
            (":ffff", (0, 65535)),
            ("ABCD:ef", (0xabcd, 0xef)),
        ] {
            assert_eq!(handle(&json!(value)), Some(expected), "{value}");
        }
        for value in [
            "", ":", "1", "-1:", ":-1", ":+1", ": 1", ":1:", ":gg", ":10000", "10000:1",
        ] {
            assert_eq!(handle(&json!(value)), None, "{value}");
        }
        for value in [Value::Null, json!(1)] {
            assert_eq!(handle(&value), None);
        }
    }

    #[test]
    fn traffic_limit_adopts_default_mq_with_iproute2_abbreviated_parents() {
        // tc prints a zero-major class ID as :1, not 0:1, even though the root is 0:.
        let mut queues = vec![json!({"kind":"mq","handle":"0:","root":true})];
        for minor in 1..=16 {
            queues.push(json!({
                "kind":"fq_codel", "handle":"0:", "parent":format!(":{minor:x}"),
                "options":{"limit":10240,"flows":1024,"ecn":true}
            }));
        }
        check(&queues, &queues[0]).unwrap();
        queues.reverse();
        check(&queues, &queues[16]).unwrap();

        queues[0]["kind"] = "tbf".into();
        assert!(check(&queues, &queues[16]).is_err());
        queues[0]["kind"] = "fq".into();
        queues[0]["options"]["maxrate"] = 1_000_000.into();
        assert!(check(&queues, &queues[16]).is_err());
        queues[0]["options"]
            .as_object_mut()
            .unwrap()
            .remove("maxrate");
        queues[0]["parent"] = ":f".into();
        assert!(check(&queues, &queues[16]).is_err());
    }

    #[test]
    fn traffic_limit_adopts_mq_with_plain_schedulers_regardless_of_root_handle() {
        for root in ["0:", "1:", "8000:"] {
            for kind in ["fq", "fq_codel", "pfifo_fast", "noqueue"] {
                let mut queues = mq();
                queues[0]["handle"] = root.into();
                for (index, queue) in queues.iter_mut().enumerate().skip(1) {
                    queue["kind"] = kind.into();
                    queue["parent"] = format!("{root}{index:x}").into();
                    // Multiple kernel-created leaves may all have handle 0:.
                    queue["handle"] = "0:".into();
                }
                queues.push(json!({"kind":"clsact","handle":"ffff:","parent":"ffff:fff1"}));
                check(&queues, &queues[0]).unwrap();
                queues.reverse();
                check(&queues, &queues[3]).unwrap();
            }
        }
        let queues = mq();
        check(&queues, &queues[0]).unwrap();
    }

    #[test]
    fn traffic_limit_rejects_shaping_or_unknown_mq_children_even_with_default_root() {
        for root in ["0:", "1:"] {
            for kind in ["htb", "tbf", "cake", "netem", "prio", "mqprio", "unknown"] {
                let mut queues = mq();
                queues[0]["handle"] = root.into();
                queues[1]["parent"] = format!("{root}1").into();
                queues[2]["parent"] = format!("{root}2").into();
                queues[2]["kind"] = kind.into();
                assert!(check(&queues, &queues[0]).is_err(), "{root} {kind}");
            }
        }
        for key in ["maxrate", "defrate"] {
            let mut queues = mq();
            queues[2]["options"][key] = 1_000_000.into();
            assert!(check(&queues, &queues[0]).is_err(), "fq {key}");
        }
    }

    #[test]
    fn traffic_limit_rejects_incomplete_or_ambiguous_mq_trees() {
        for (index, key, value) in [
            (0, "handle", "-1:"),
            (0, "handle", "1:1"),
            (0, "handle", "10000:"),
            (1, "parent", "2:1"),
            (1, "parent", "1:"),
            (1, "parent", "1:2"),
            (1, "parent", "1:3"),
            (1, "handle", "8002:"),
        ] {
            let mut queues = mq();
            queues[index][key] = value.into();
            assert!(check(&queues, &queues[0]).is_err(), "{index} {key} {value}");
        }
        let queues = mq();
        assert!(check(&queues[..1], &queues[0]).is_err());
        let mut queues = mq();
        queues.push(json!({"kind":"htb","handle":"2:","root":true}));
        assert!(check(&queues, &queues[0]).is_err());
    }

    #[test]
    fn traffic_limit_adopts_plain_schedulers_by_behavior_instead_of_handle() {
        for kind in [
            "fq",
            "fq_codel",
            "pfifo_fast",
            "noqueue",
            "pfifo",
            "bfifo",
            "sfq",
            "codel",
            "pie",
            "fq_pie",
        ] {
            for handle in ["0:", "1:", "1234:", "8001:", "8100:"] {
                let mut root = json!({"kind":kind,"handle":handle,"root":true,"options":{}});
                check(std::slice::from_ref(&root), &root).unwrap();
                root["options"]["maxrate"] = 1_000_000.into();
                assert!(check(std::slice::from_ref(&root), &root).is_err());
            }
        }
    }

    #[test]
    fn traffic_limit_rejects_external_root_policies_independent_of_handle() {
        for kind in ["htb", "tbf", "cake", "netem", "prio", "mqprio", "unknown"] {
            for handle in ["0:", "1:", "8001:"] {
                let root = json!({"kind":kind,"handle":handle,"root":true});
                assert!(
                    check(std::slice::from_ref(&root), &root).is_err(),
                    "{kind} {handle}"
                );
            }
        }
    }

    #[test]
    fn traffic_limit_failed_adoption_preserves_kernel_and_checkpoint() {
        use super::super::{reconcile_network, NETWORK};
        struct ReadOnly {
            failed_read: bool,
            default_root: bool,
        }
        impl Network for ReadOnly {
            fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
                match (program, args) {
                    ("tc", ["-j", "qdisc", "show", "dev", "eth0"]) => {
                        let mut queues = mq();
                        if self.default_root {
                            queues[0]["handle"] = "0:".into();
                            queues[1]["parent"] = ":1".into();
                            queues[2]["parent"] = ":2".into();
                        }
                        Ok(serde_json::to_string(&queues).unwrap())
                    }
                    ("tc", ["-j", "filter", "show", "dev", "eth0", "parent", "8002:"]) => {
                        if self.failed_read {
                            Err("filter read failed".into())
                        } else {
                            Ok(r#"[{"kind":"matchall"}]"#.into())
                        }
                    }
                    ("tc", ["-j", "filter", "show", "dev", "eth0", "parent", _]) => Ok("[]".into()),
                    _ => panic!("failed adoption must not touch the network: {program} {args:?}"),
                }
            }
        }
        let dir =
            std::env::temp_dir().join(format!("brocade-limit-adoption-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for default_root in [false, true] {
            for failed_read in [false, true] {
                let error = reconcile_network(
                    &dir,
                    Some("eth0"),
                    Some(30_000_000),
                    &mut ReadOnly {
                        failed_read,
                        default_root,
                    },
                )
                .unwrap_err();
                assert!(
                    error.contains(if failed_read {
                        "filter read failed"
                    } else {
                        "8002: 已有自定义过滤"
                    }),
                    "{error}"
                );
                assert!(!dir.join(NETWORK).exists());
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn traffic_limit_checks_every_named_scheduler_filter_before_mutation() {
        struct Filters {
            seen: Vec<String>,
            fail: bool,
        }
        impl Network for Filters {
            fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
                assert_eq!(program, "tc");
                assert_eq!(
                    &args[..6],
                    &["-j", "filter", "show", "dev", "eth0", "parent"]
                );
                let parent = args[6];
                self.seen.push(parent.into());
                if self.fail {
                    return Err("filter dump failed".into());
                }
                Ok(if parent == "8002:" {
                    r#"[{"kind":"matchall"}]"#
                } else {
                    "[]"
                }
                .into())
            }
        }
        let mut network = Filters {
            seen: Vec::new(),
            fail: false,
        };
        let queues = mq();
        assert!(check_filters(&mut network, "eth0", &queues)
            .unwrap_err()
            .contains("8002:"));
        assert_eq!(network.seen, ["1:", "8001:", "8002:"]);
        network.fail = true;
        assert_eq!(
            check_filters(&mut network, "eth0", &queues).unwrap_err(),
            "filter dump failed"
        );
    }
}
