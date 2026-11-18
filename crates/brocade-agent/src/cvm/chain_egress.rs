//! Managed subscriber clients. Marked primary traffic has two independent terminal guards:
//! an unreachable RPDB route and a forward firewall rule that admits only its owned TUN.
use crate::{
    command::{run_command, run_command_with_input},
    fsutil::{atomic_write_private, read_bounded},
    options::Options,
};
use brocade_deployment::cvm::*;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
#[derive(Default)]
struct Manager {
    clients: BTreeMap<String, Client>,
    restored: bool,
}
struct Client {
    artifact: ChainClientArtifact,
    tun: File,
    child: Option<Child>,
    root: PathBuf,
    error: Option<String>,
}
static CLIENTS: OnceLock<Mutex<Manager>> = OnceLock::new();
fn manager() -> &'static Mutex<Manager> {
    CLIENTS.get_or_init(Mutex::default)
}
fn key(a: &ChainClientArtifact) -> String {
    format!("{}:{}", a.instance_id, a.chain_id)
}
fn table(a: &ChainClientArtifact) -> String {
    format!("brocade_chain_{}", &a.binding.device[3..])
}
fn alias(a: &ChainClientArtifact) -> String {
    format!("brocade-chain:{}:{}", a.instance_id, a.chain_id)
}
fn command(args: &[&str]) -> Result<String> {
    // These arguments contain only validated kernel object identities, never credentials.
    run_command("ip", args).map_err(|error| {
        format!(
            "线路出口系统网络配置失败：{}",
            error
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(1024)
                .collect::<String>()
        )
    })
}
fn kernel_number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        let text = value.as_str()?;
        text.strip_prefix("0x")
            .map(|hex| u64::from_str_radix(hex, 16).ok())
            .unwrap_or_else(|| text.parse().ok())
    })
}
fn bounded_error(error: &str) -> String {
    let mut end = error.len().min(500);
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    error[..end].to_owned()
}
fn stop(child: &mut Option<Child>) -> Result<()> {
    if let Some(mut process) = child.take() {
        if process
            .try_wait()
            .map_err(|_| "无法读取线路客户端状态")?
            .is_none()
        {
            process.kill().map_err(|_| "无法停止线路客户端")?;
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if process
                    .try_wait()
                    .map_err(|_| "无法回收线路客户端")?
                    .is_some()
                {
                    break;
                };
                if Instant::now() >= until {
                    return Err("线路客户端回收未确认".into());
                };
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    Ok(())
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = stop(&mut self.child);
    }
}
fn validate(a: &ChainClientArtifact) -> Result<()> {
    if !valid_id(&a.instance_id)
        || !valid_id(&a.chain_id)
        || !valid_id(&a.user_id)
        || a.content.len() > 1024 * 1024
        || a.sha256 != brocade_core::hash::sha256_hex(a.content.as_bytes())
        || a.binding != brocade_cvm_compile::chain_binding(&a.instance_id, &a.chain_id)
        || a.guard != brocade_cvm_compile::host_network::chain_guard(a, false)
    {
        return Err("线路客户端冻结产物校验失败".into());
    }
    let v: Value = serde_json::from_str(&a.content).map_err(|_| "线路客户端产物不是有效 JSON")?;
    if v["inbounds"].as_array().is_none_or(|x| x.len() != 1)
        || v["inbounds"][0]["protocol"] != "tun"
        || v["inbounds"][0]["settings"]["name"] != a.binding.device
        || v["outbounds"]
            .as_array()
            .is_none_or(|x| x.is_empty() || x.iter().any(|o| o["protocol"] == "freedom"))
    {
        return Err("线路客户端产物含未授权的入口或直接出口".into());
    };
    Ok(())
}
fn open_tun(a: &ChainClientArtifact, root: &Path) -> Result<File> {
    let device = &a.binding.device;
    let checkpoint = root.join("owner.json");
    let old = read_bounded(&checkpoint, 8192)?;
    let expected = json!({"instance_id":a.instance_id,"chain_id":a.chain_id,"device":device,"mark":a.binding.mark});
    let links = command(&["-j", "link", "show"])?;
    let links: Vec<Value> = serde_json::from_str(&links).map_err(|_| "线路出口网卡清单无效")?;
    if let Some(link) = links.iter().find(|l| l["ifname"] == *device) {
        if old
            .as_ref()
            .and_then(|b| serde_json::from_slice::<Value>(b).ok())
            != Some(expected.clone())
            || link["ifalias"] != alias(a)
        {
            return Err("线路出口设备已有其他所有者".into());
        }
    } else {
        if old
            .as_ref()
            .is_some_and(|b| serde_json::from_slice::<Value>(b).ok() != Some(expected.clone()))
        {
            return Err("线路出口所有权检查点不匹配".into());
        }
        atomic_write_private(
            &checkpoint,
            &serde_json::to_vec(&expected).map_err(|_| "线路所有权编码失败")?,
        )?;
        command(&["tuntap", "add", "dev", device, "mode", "tun"])?;
        command(&["link", "set", "dev", device, "alias", &alias(a)])?;
    }
    #[repr(C)]
    struct Request {
        name: [u8; 16],
        flags: i16,
        padding: [u8; 22],
    }
    let mut request = Request {
        name: [0; 16],
        flags: 0x1001,
        padding: [0; 22],
    };
    request.name[..device.len()].copy_from_slice(device.as_bytes());
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open("/dev/net/tun")
        .map_err(|_| "无法打开线路 TUN")?;
    // SAFETY: Request has Linux ifreq layout; the file remains owned for the ioctl and child lifetime.
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x400454ca as _, &mut request) } < 0 {
        return Err("无法绑定已核验的线路 TUN".into());
    }
    command(&[
        "link",
        "set",
        "dev",
        device,
        "mtu",
        &CHAIN_TUN_MTU.to_string(),
    ])?;
    Ok(file)
}
fn owned_chain_device(devices: &[Value], name: &Value) -> bool {
    devices.iter().any(|device| {
        device["ifname"] == *name
            && device["ifalias"]
                .as_str()
                .is_some_and(|alias| alias.starts_with("brocade-chain:"))
    })
}
fn network(a: &ChainClientArtifact) -> Result<()> {
    super::nics::enable_forwarding()?;
    let mark = a.binding.mark.to_string();
    let device = &a.binding.device;
    // Linux rejects reverse-path validation on an unnumbered IPv4 input device even in loose
    // mode. Give only the owned TUN a link-local /32; leave host-wide rp_filter unchanged.
    let devices: Vec<Value> = serde_json::from_str(&command(&["-j", "link", "show"])?)
        .map_err(|_| "线路设备归属清单无效")?;
    let links: Vec<Value> = serde_json::from_str(&command(&["-4", "-j", "address", "show"])?)
        .map_err(|_| "线路本地地址清单无效")?;
    for link in &links {
        for address in link["addr_info"].as_array().into_iter().flatten() {
            if address["local"] == CHAIN_TUN_IPV4 {
                if link["ifname"] != *device && !owned_chain_device(&devices, &link["ifname"]) {
                    return Err("线路 TUN 的本地链路地址已被其他接口占用".into());
                }
            } else if link["ifname"] == *device {
                return Err("线路 TUN 上存在非受管 IPv4 地址".into());
            }
        }
    }
    command(&[
        "-4",
        "address",
        "replace",
        &format!("{CHAIN_TUN_IPV4}/32"),
        "dev",
        device,
        "scope",
        "link",
        "noprefixroute",
    ])?;
    for family in ["-4", "-6"] {
        let all = command(&["-N", family, "-j", "route", "show", "table", "all"])?;
        let routes: Vec<Value> = serde_json::from_str(&all).map_err(|_| "线路路由表清单无效")?;
        for route in routes
            .iter()
            .filter(|r| kernel_number(&r["table"]) == Some(u64::from(a.binding.mark)))
        {
            if kernel_number(&route["protocol"]) != Some(186) {
                return Err("线路专用路由表已被其他配置占用".into());
            }
            if !(kernel_number(&route["type"]) == Some(7) && route["metric"] == 42760
                || route["dev"] == *device && route["metric"] == 10)
            {
                return Err("线路专用路由表存在非受管路径".into());
            }
        }
        command(&[
            family,
            "route",
            "replace",
            "unreachable",
            "default",
            "metric",
            "42760",
            "proto",
            "186",
            "table",
            &mark,
        ])?;
        let raw = command(&["-N", family, "-j", "rule", "show"])?;
        let rules: Vec<Value> = serde_json::from_str(&raw).map_err(|_| "线路选路规则清单无效")?;
        let has = rules.iter().any(|r| {
            r["priority"] == 28800
                && kernel_number(&r["table"]) == Some(u64::from(a.binding.mark))
                && kernel_number(&r["fwmark"]) == Some(u64::from(a.binding.mark))
        });
        if !has {
            command(&[
                family,
                "rule",
                "add",
                "pref",
                "28800",
                "fwmark",
                &format!("{:#x}/0xffffffff", a.binding.mark),
                "lookup",
                &mark,
            ])?;
        }
        if a.blocked {
            // A down persistent TUN also invalidates its unicast route; the unreachable route stays.
            command(&["link", "set", "dev", device, "down"])?;
        } else {
            command(&["link", "set", "dev", device, "up"])?;
            command(&[
                family, "route", "replace", "default", "dev", device, "metric", "10", "proto",
                "186", "table", &mark,
            ])?;
        }
    }
    let name = table(a);
    let comment = alias(a);
    // Atomic replacement: a live client never gets an interval with an empty guard chain.
    let exists = crate::command::capture_command_with_timeout(
        "nft",
        &["-j", "list", "table", "inet", &name],
        Duration::from_secs(5),
    )?;
    if exists.status.success() {
        let v: Value = serde_json::from_slice(&exists.stdout).map_err(|_| "线路防火墙清单无效")?;
        if !v["nftables"]
            .as_array()
            .is_some_and(|items| items.iter().any(|x| x["table"]["comment"] == comment))
        {
            return Err("线路防火墙表已有其他所有者".into());
        }
    }
    let script = format!(
        "{}{}",
        if exists.status.success() {
            format!("delete table inet {name}\n")
        } else {
            String::new()
        },
        a.guard
    );
    run_command_with_input("nft", &["-f", "-"], script.as_bytes()).map_err(|error| {
        format!(
            "线路出口防回落规则应用失败：{}",
            error
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(1024)
                .collect::<String>()
        )
    })?;
    Ok(())
}
fn start(options: &Options, a: &ChainClientArtifact, root: &Path, tun: &File) -> Result<Child> {
    let path = root.join("xray.json");
    atomic_write_private(&path, a.content.as_bytes())?;
    let binary = crate::options::xray_binary_path();
    let text = path.to_str().ok_or("线路配置路径无效")?;
    crate::command::capture_command_with_timeout(
        binary.to_str().ok_or("Xray 路径无效")?,
        &["run", "-test", "-config", text],
        Duration::from_secs(15),
    )?
    .status
    .success()
    .then_some(())
    .ok_or("Xray 不支持所选线路客户端产物")?;
    let fd = tun.as_raw_fd();
    let parent = std::process::id() as libc::pid_t;
    let mut cmd = Command::new(binary);
    cmd.args(["run", "-config", text])
        .env("XRAY_TUN_FD", "3")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    super::diagnostics::capture(
        &mut cmd,
        &options.state_dir,
        &a.instance_id,
        NetworkLogSource::Xray,
        super::diagnostics::secrets(std::slice::from_ref(&a.content)),
    );
    // SAFETY: only async-signal-safe descriptor and process-control operations run after fork.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 || libc::getppid() != parent {
                return Err(std::io::Error::last_os_error());
            }
            if fd != 3 && libc::dup2(fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            };
            Ok(())
        });
    }
    cmd.spawn().map_err(|_| "线路客户端启动失败".into())
}

pub(super) fn sync(options: &Options, artifacts: &[ChainClientArtifact]) -> Result<()> {
    if artifacts.len() > 1024 {
        return Err("线路客户端数量超出本机上限".into());
    }
    let mut marks = BTreeSet::new();
    for a in artifacts {
        validate(a)?;
        if !marks.insert(a.binding.mark) {
            return Err("线路客户端执行标识冲突".into());
        }
    }
    let mut all = manager().lock().map_err(|_| "线路客户端状态不可用")?;
    if !all.restored {
        let root = options.state_dir.join("cvm-chain-egress");
        if root.exists() {
            let paths = std::fs::read_dir(&root).map_err(|_| "无法读取线路客户端检查点")?;
            for (index, entry) in paths.enumerate() {
                if index >= 1024 {
                    return Err("线路客户端检查点数量超出上限".into());
                }
                let entry = entry.map_err(|_| "无法读取线路客户端检查点")?;
                if !entry
                    .file_type()
                    .map_err(|_| "线路检查点类型无效")?
                    .is_dir()
                {
                    return Err("线路检查点必须是独立目录".into());
                }
                let root = entry.path();
                let Some(bytes) = read_bounded(&root.join("artifact.json"), 2 * 1024 * 1024)?
                else {
                    continue;
                };
                let a: ChainClientArtifact =
                    serde_json::from_slice(&bytes).map_err(|_| "线路客户端检查点无效")?;
                validate(&a)?;
                if root.file_name().and_then(|n| n.to_str()) != Some(&a.binding.device) {
                    return Err("线路客户端检查点归属不一致".into());
                }
                let tun = open_tun(&a, &root)?;
                command(&["link", "set", "dev", &a.binding.device, "down"])?;
                all.clients.insert(
                    key(&a),
                    Client {
                        artifact: a,
                        tun,
                        child: None,
                        root,
                        error: None,
                    },
                );
            }
        }
        all.restored = true;
    }
    for a in artifacts {
        let id = key(a);
        let root = options
            .state_dir
            .join("cvm-chain-egress")
            .join(&a.binding.device);
        std::fs::create_dir_all(&root).map_err(|_| "无法创建线路客户端状态目录")?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "无法保护线路客户端状态目录")?;
        if let std::collections::btree_map::Entry::Vacant(entry) = all.clients.entry(id.clone()) {
            let tun = open_tun(a, &root)?;
            entry.insert(Client {
                artifact: a.clone(),
                tun,
                child: None,
                root,
                error: None,
            });
        }
        let client = all.clients.get_mut(&id).ok_or("线路客户端状态丢失")?;
        if client.artifact != *a {
            stop(&mut client.child)?;
            client.artifact = a.clone();
        }
        atomic_write_private(
            &client.root.join("artifact.json"),
            &serde_json::to_vec(a).map_err(|_| "线路客户端检查点编码失败")?,
        )?;
        let result = (|| {
            network(a)?;
            if a.blocked {
                stop(&mut client.child)?;
                return Ok(());
            }
            if client
                .child
                .as_mut()
                .is_some_and(|p| p.try_wait().ok().flatten().is_some())
            {
                client.child = None;
            }
            if client.child.is_none() {
                client.child = Some(start(options, a, &client.root, &client.tun)?);
            }
            Ok(())
        })();
        client.error = result.err();
        if let Some(error) = &client.error {
            super::diagnostics::record(
                &options.state_dir,
                Some(&a.instance_id),
                NetworkLogSource::Agent,
                NetworkLogLevel::Error,
                error,
            );
        }
    }
    Ok(())
}

pub(super) fn ready(instance: &str, routes: &ContainerRoutes) -> bool {
    let expected = routes.chain_ids();
    let actual = receipts(instance);
    expected
        .iter()
        .all(|id| actual.iter().any(|c| c.chain_id == *id && c.configured))
}

/// Retire only after the runtime acknowledged the new selector (or a stopped/deleted guest).
/// Until then the old route retains its unreachable path and firewall guard across restarts.
pub(super) fn retire(_options: &Options, desired: &InstanceDesired) -> Result<()> {
    let keep = if desired.power == PowerState::Running {
        desired.spec.routing.chain_ids()
    } else {
        BTreeSet::new()
    };
    let mut all = manager().lock().map_err(|_| "线路客户端状态不可用")?;
    let retired = all
        .clients
        .iter()
        .filter(|(_, c)| {
            c.artifact.instance_id == desired.id && !keep.contains(c.artifact.chain_id.as_str())
        })
        .map(|(k, _)| k.clone())
        .collect::<Vec<_>>();
    for id in retired {
        let client = all.clients.get_mut(&id).ok_or("线路客户端检查点丢失")?;
        let a = &client.artifact;
        stop(&mut client.child)?;
        command(&["link", "set", "dev", &a.binding.device, "down"])?;
        // Revalidate every owned kernel object before deleting any of them.
        let mut blocked = a.clone();
        blocked.blocked = true;
        network(&blocked)?;
        for family in ["-4", "-6"] {
            command(&[
                family,
                "rule",
                "del",
                "pref",
                "28800",
                "fwmark",
                &format!("{:#x}/0xffffffff", a.binding.mark),
                "lookup",
                &a.binding.mark.to_string(),
            ])?;
            command(&[
                family,
                "route",
                "flush",
                "table",
                &a.binding.mark.to_string(),
                "proto",
                "186",
            ])?;
        }
        run_command("nft", &["delete", "table", "inet", &table(a)])
            .map_err(|_| "线路防回落规则清理失败")?;
        command(&["link", "delete", "dev", &a.binding.device])?;
        std::fs::remove_dir_all(&client.root).map_err(|_| "线路检查点清理失败")?;
        all.clients.remove(&id);
    }
    Ok(())
}

pub(super) fn receipts(instance: &str) -> Vec<ChainClientReceipt> {
    let Ok(mut all) = manager().lock() else {
        return Vec::new();
    };
    all.clients
        .values_mut()
        .filter(|c| c.artifact.instance_id == instance)
        .map(|c| {
            let alive = c.artifact.blocked
                || c.child
                    .as_mut()
                    .is_some_and(|p| matches!(p.try_wait(), Ok(None)));
            ChainClientReceipt {
                chain_id: c.artifact.chain_id.clone(),
                sha256: c.artifact.sha256.clone(),
                guard_sha256: brocade_core::hash::sha256_hex(c.artifact.guard.as_bytes()),
                configured: c.error.is_none() && alive,
                error: c
                    .error
                    .as_deref()
                    .map(bounded_error)
                    .or_else(|| (!alive).then(|| "线路客户端未运行；保持阻断".into())),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn address_inventory_without_alias_recognizes_other_owned_chain_links() {
        let devices = vec![
            json!({"ifname":"bcg-one","ifalias":"brocade-chain:cvm-one:chain-one"}),
            json!({"ifname":"foreign0"}),
        ];
        assert!(owned_chain_device(&devices, &json!("bcg-one")));
        assert!(!owned_chain_device(&devices, &json!("foreign0")));
        assert!(!owned_chain_device(&devices, &json!("missing0")));
    }
    #[test]
    fn iproute_numbers_accept_numeric_strings_and_hex_marks() {
        let rule: Value = serde_json::from_str(
            r#"{"priority":28800,"fwmark":"0xb4f7f23f","table":"3036148287"}"#,
        )
        .unwrap();
        assert_eq!(kernel_number(&rule["table"]), Some(3036148287));
        assert_eq!(kernel_number(&rule["fwmark"]), Some(3036148287));
        assert_eq!(kernel_number(&serde_json::json!(186)), Some(186));
        assert_eq!(kernel_number(&serde_json::json!("186")), Some(186));
        assert_eq!(kernel_number(&serde_json::json!("bgp")), None);
    }
}
