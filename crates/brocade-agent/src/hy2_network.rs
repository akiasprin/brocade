//! Restart-only HY2 networking. The public artifact remains unchanged; this module
//! owns only its local realization and never moves the serving process into netns.
use crate::{command::run_command_with_timeout, fsutil, hy2_port_hop, DesiredArtifact};
use brocade_core::hash::sha256_hex;
use hy2_port_hop::Redirect;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

mod ports;
#[cfg(target_os = "linux")]
mod timestamp;
#[cfg(not(target_os = "linux"))]
mod timestamp {
    pub(super) fn preflight() -> Result<(), String> {
        Err("HY2 隔离需要 Linux 时间戳过滤器".into())
    }
    pub(super) fn ensure(_: &str) -> Result<(), String> {
        preflight()
    }
    pub(super) fn matches(_: &str) -> Result<bool, String> {
        preflight().map(|_| false)
    }
}
#[cfg(test)]
mod tests;

pub(crate) const ENV: &str = "XRAY_BROCADE_HY2_NAMESPACE";
pub(crate) const CAPABILITY: &str = "hy2-netns-v1";
const STATE: &str = "hy2-network.json";
const POLICY: &str = "hy2-namespace.json";
const PENDING: &str = "hy2-network.pending.json";
const TABLE: &str = "brocade_hy2_transport";
const HOST_IP: &str = "169.254.254.1";
const PEER_IP: &str = "169.254.254.2";
const HOST_IP6: &str = "fd42:b10c:adf0::1";
const PEER_IP6: &str = "fd42:b10c:adf0::2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Spec {
    redirects: Vec<Redirect>,
    listeners: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NetworkState {
    version: u32,
    boot_id: String,
    owner: String,
    namespace: String,
    interface: String,
    namespace_inode: u64,
    phase: String,
    spec: Spec,
    #[serde(default)]
    rules_sha256: String,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    xray_sha256: String,
    content: String,
}

#[derive(Serialize)]
struct SocketPolicy<'a> {
    version: u32,
    namespace_path: String,
    namespace_inode: u64,
    namespace_device: u64,
    boot_id: &'a str,
    listeners: &'a [String],
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    run_command_with_timeout(program, args, Duration::from_secs(5))
}

fn nft(script: &str) -> Result<(), String> {
    // Every interpolated value is an internally generated name, address or u16.
    crate::run_shell_with_timeout(
        &format!("set -eu\nnft -f - <<'BROCADE_NFT'\n{script}\nBROCADE_NFT\n"),
        Duration::from_secs(5),
    )
    .map(|_| ())
}

fn net(namespace: &str, program: &str, args: &[&str]) -> Result<String, String> {
    let mut command = vec!["netns", "exec", namespace, program];
    command.extend_from_slice(args);
    run("ip", &command)
}

fn identity(state_dir: &Path) -> Result<(String, String, String), String> {
    let canonical = fs::canonicalize(state_dir).map_err(|e| e.to_string())?;
    let owner = sha256_hex(canonical.as_os_str().as_encoded_bytes());
    Ok((
        format!("brocade-hy2-{}", &owner[..12]),
        format!("bh2{}", &owner[..8]),
        owner,
    ))
}

fn boot_id() -> Result<String, String> {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|text| text.trim().to_owned())
        .map_err(|e| e.to_string())
}

fn write_json(state_dir: &Path, name: &str, value: &impl Serialize) -> Result<(), String> {
    fsutil::atomic_write_private(
        &state_dir.join(name),
        &serde_json::to_vec(value).map_err(|e| e.to_string())?,
    )
}

fn optional_file(path: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 1024 * 1024 =>
        {
            fs::read_to_string(path)
                .map(Some)
                .map_err(|e| e.to_string())
        }
        Ok(_) => Err(format!("拒绝不安全的 HY2 状态文件 {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn read_state(state_dir: &Path) -> Result<Option<NetworkState>, String> {
    let Some(text) = optional_file(&state_dir.join(STATE))? else {
        return Ok(None);
    };
    let state: NetworkState =
        serde_json::from_str(&text).map_err(|e| format!("HY2 网络检查点损坏：{e}"))?;
    let (namespace, interface, owner) = identity(state_dir)?;
    if state.version != 1
        || state.namespace != namespace
        || state.interface != interface
        || state.owner != owner
        || !matches!(state.phase.as_str(), "preparing" | "ready" | "active")
    {
        return Err("HY2 网络检查点身份不匹配".into());
    }
    Ok(Some(state))
}

pub(crate) fn active(state_dir: &Path) -> bool {
    read_state(state_dir)
        .ok()
        .flatten()
        .is_some_and(|s| s.phase == "active" && boot_id().ok().as_deref() == Some(&s.boot_id))
}

pub(crate) fn managed(state_dir: &Path) -> Result<bool, String> {
    Ok(read_state(state_dir)?.is_some())
}

pub(crate) fn needs_recovery(state_dir: &Path, config: &str) -> Result<bool, String> {
    let Some(state) = read_state(state_dir)? else {
        return Ok(false);
    };
    restore_namespace_handle(state_dir, &state)?;
    let namespace = fs::metadata(format!("/run/netns/{}", state.namespace));
    let wanted = specification(config, &hop_content(state_dir, config)?)?;
    let restart = state.phase != "active"
        || state.boot_id != boot_id()?
        || namespace.map_or(true, |m| m.ino() != state.namespace_inode)
        || wanted.is_none_or(|s| s.listeners != state.spec.listeners);
    if restart {
        return Ok(true);
    }
    let links: Value = serde_json::from_str(&run("ip", &["-j", "-d", "addr", "show"])?)
        .map_err(|e| e.to_string())?;
    let Some(link) = links
        .as_array()
        .ok_or("无法读取 HY2 链路状态")?
        .iter()
        .find(|l| l["ifname"] == state.interface)
    else {
        return Ok(true);
    };
    if link["ifalias"] != state.owner {
        return Err("HY2 链路归属发生变化，拒绝覆盖".into());
    }
    let valid_link = |link: &Value, v4: &str, v6: &str| {
        link["flags"]
            .as_array()
            .is_some_and(|flags| flags.iter().any(|v| v == "UP"))
            && link["addr_info"].as_array().is_some_and(|addresses| {
                [(v4, 30), (v6, 64)].iter().all(|(address, prefix)| {
                    addresses
                        .iter()
                        .any(|a| a["local"] == *address && a["prefixlen"] == *prefix)
                })
            })
    };
    if !valid_link(link, HOST_IP, HOST_IP6) {
        return Ok(true);
    }
    let peer = format!("{}p", state.interface);
    let links: Value = serde_json::from_str(&net(&state.namespace, "ip", &["-j", "addr", "show"])?)
        .map_err(|e| e.to_string())?;
    if !links
        .as_array()
        .ok_or("无法读取 HY2 对端链路")?
        .iter()
        .any(|l| l["ifname"] == peer && valid_link(l, PEER_IP, PEER_IP6))
    {
        return Ok(true);
    }
    for (family, destination, gateway) in [
        ("-4", "169.254.254.0/30", HOST_IP),
        ("-6", "fd42:b10c:adf0::/64", HOST_IP6),
    ] {
        let routes: Value = serde_json::from_str(&run(
            "ip",
            &["-j", family, "route", "show", "dev", &state.interface],
        )?)
        .map_err(|e| e.to_string())?;
        if !routes
            .as_array()
            .ok_or("无法读取 HY2 直连路由")?
            .iter()
            .any(|r| r["dst"] == destination)
        {
            return Ok(true);
        }
        let routes: Value = serde_json::from_str(&net(
            &state.namespace,
            "ip",
            &["-j", family, "route", "show", "default"],
        )?)
        .map_err(|e| e.to_string())?;
        if !routes
            .as_array()
            .ok_or("无法读取 HY2 回程路由")?
            .iter()
            .any(|r| r["gateway"] == gateway && r["dev"] == peer)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

// LogNamespace gives a restarted Agent a new mount namespace. The serving
// Xray still pins the original named netns in its mount view. Rebind that exact
// handle instead of treating the underlying empty mountpoint as a new netns.
fn restore_namespace_handle(state_dir: &Path, state: &NetworkState) -> Result<(), String> {
    if state.namespace_inode == 0 || state.boot_id != boot_id()? {
        return Ok(());
    }
    let path = Path::new("/run/netns").join(&state.namespace);
    let device = fs::metadata("/proc/self/ns/net")
        .map_err(|e| e.to_string())?
        .dev();
    let target = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.dev() == device {
                return if metadata.ino() == state.namespace_inode {
                    Ok(())
                } else {
                    Err("HY2 netns 身份变化，拒绝覆盖".into())
                };
            }
            if !metadata.is_file() || metadata.len() != 0 || metadata.uid() != 0 {
                return Err("HY2 netns 挂载点不是受管的空文件，拒绝覆盖".into());
            }
            Some(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    for process in crate::serving_xray_processes_in(state_dir) {
        let source = format!("/proc/{}/root/run/netns/{}", process.pid, state.namespace);
        let handle = match fs::File::open(&source) {
            Ok(handle) => handle,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("无法读取受管 Xray 的 HY2 空间句柄：{error}")),
        };
        let identity = handle.metadata().map_err(|e| e.to_string())?;
        if identity.dev() != device
            || identity.ino() != state.namespace_inode
            || !crate::process_is_live(process)
        {
            continue;
        }
        if target.is_none() {
            fs::create_dir_all("/run/netns").map_err(|e| e.to_string())?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| e.to_string())?;
        }
        // Keep the verified namespace open across the mount subprocess, so PID
        // reuse or an exiting Xray cannot change which namespace gets bound.
        bind_namespace_handle(&handle, &path)?;
        let restored = fs::metadata(&path).map_err(|e| e.to_string())?;
        if restored.dev() != device || restored.ino() != state.namespace_inode {
            return Err("HY2 空间句柄恢复后的身份不匹配".into());
        }
        return Ok(());
    }
    // With no surviving pin, normal recovery rebuilds the owned link. The
    // empty underlying file is not a network namespace and must not block it.
    if target.is_some() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bind_namespace_handle(handle: &fs::File, path: &Path) -> Result<(), String> {
    use std::{ffi::CString, os::fd::AsRawFd};
    let source = CString::new(format!("/proc/self/fd/{}", handle.as_raw_fd()))
        .map_err(|_| "HY2 空间句柄路径包含 NUL")?;
    let target =
        CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| "HY2 空间挂载点包含 NUL")?;
    // libmount canonicalizes procfs FD paths back to the original pathname,
    // which names an empty mountpoint in the new Agent's mount namespace.
    // SAFETY: Both strings stay alive for this call, the File pins the verified
    // namespace, and MS_BIND uses neither a filesystem type nor filesystem data.
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    if result != 0 {
        return Err(format!(
            "恢复 HY2 空间句柄失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn bind_namespace_handle(_handle: &fs::File, _path: &Path) -> Result<(), String> {
    Err("HY2 网络空间需要 Linux".into())
}

pub(crate) fn restart_required(state_dir: &Path) -> bool {
    read_state(state_dir).map_or(true, |s| s.is_some())
        || optional_file(&state_dir.join("hy2_port_hop.json"))
            .ok()
            .flatten()
            .is_some_and(|text| {
                !state_dir.join("hy2_port_hop.disabled").exists()
                    && hy2_port_hop::parse(&text).is_ok_and(|r| !r.is_empty())
            })
}

pub(crate) fn stage(
    state_dir: &Path,
    hops: &DesiredArtifact,
    xray: &DesiredArtifact,
) -> Result<(), String> {
    let current;
    let xray = match xray {
        DesiredArtifact::Present { content, .. } => content.as_str(),
        DesiredArtifact::Unmanaged { .. } => {
            if !state_dir.join("xray.json").exists() {
                return Ok(());
            }
            current = fs::read_to_string(state_dir.join("xray.json")).map_err(|e| e.to_string())?;
            current.as_str()
        }
        DesiredArtifact::Disabled { .. } => return Ok(()),
    };
    let content = match hops {
        DesiredArtifact::Present { content, .. } => content.as_str(),
        DesiredArtifact::Disabled { .. } => "{}",
        DesiredArtifact::Unmanaged { .. } => return Ok(()),
    };
    // Validate before altering even the pending local checkpoint.
    hy2_port_hop::parse(content)?;
    write_json(
        state_dir,
        PENDING,
        &Pending {
            xray_sha256: sha256_hex(xray.as_bytes()),
            content: content.to_owned(),
        },
    )
}

fn hop_content(state_dir: &Path, xray: &str) -> Result<String, String> {
    if let Some(pending) = optional_file(&state_dir.join(PENDING))? {
        let pending: Pending = serde_json::from_str(&pending).map_err(|e| e.to_string())?;
        if pending.xray_sha256 == sha256_hex(xray.as_bytes()) {
            return Ok(pending.content);
        }
    }
    if state_dir.join("hy2_port_hop.disabled").exists() {
        return Ok("{}".into());
    }
    Ok(optional_file(&state_dir.join("hy2_port_hop.json"))?.unwrap_or_else(|| "{}".into()))
}

fn specification(xray: &str, content: &str) -> Result<Option<Spec>, String> {
    let mut redirects = hy2_port_hop::parse(content)?;
    if redirects.is_empty() {
        return Ok(None);
    }
    if redirects.len() > 128 {
        return Err("HY2 跳跃范围数量超限".into());
    }
    redirects.sort_by_key(|r| (r.start, r.end, r.to));
    redirects.dedup();
    for (index, range) in redirects.iter().enumerate() {
        if range.start == 0
            || range.start > range.end
            || range.to < range.start
            || range.to > range.end
            || (index > 0 && redirects[index - 1].end >= range.start)
        {
            return Err("HY2 跳跃范围无效或相互重叠".into());
        }
    }
    let value: Value =
        serde_json::from_str(xray).map_err(|e| format!("HY2 监听配置解析失败：{e}"))?;
    let inbounds = value
        .get("inbounds")
        .and_then(Value::as_array)
        .ok_or("HY2 配置缺少 inbounds")?;
    let mut listeners = BTreeSet::new();
    let mut found = BTreeSet::new();
    for inbound in inbounds {
        let Some(port) = inbound
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|n| u16::try_from(n).ok())
        else {
            continue;
        };
        if !redirects.iter().any(|r| r.to == port) {
            continue;
        }
        let transport = inbound
            .pointer("/streamSettings/network")
            .and_then(Value::as_str)
            .unwrap_or("tcp");
        if transport != "hysteria" {
            continue;
        }
        let listen = inbound
            .get("listen")
            .and_then(Value::as_str)
            .unwrap_or("0.0.0.0");
        if !matches!(listen, "::" | "[::]" | "0.0.0.0") {
            return Err("HY2 隔离需要通配监听；显式地址继续使用旧网络模式".into());
        }
        if let Some(options) = inbound
            .pointer("/streamSettings/sockopt")
            .and_then(Value::as_object)
        {
            for name in [
                "interface",
                "mark",
                "tproxy",
                "customSockopt",
                "dialerProxy",
                "acceptProxyProtocol",
            ] {
                if options
                    .get(name)
                    .is_some_and(|v| !v.is_null() && v != false && v != 0 && v != "")
                {
                    return Err(format!("HY2 隔离无法保持 sockopt.{name} 的宿主机语义"));
                }
            }
        }
        listeners.insert(if listen == "0.0.0.0" {
            format!("0.0.0.0:{port}")
        } else {
            format!("[::]:{port}")
        });
        found.insert(port);
    }
    if redirects.iter().any(|r| !found.contains(&r.to)) {
        return Err("HY2 跳跃目标没有对应的 Hysteria UDP 监听".into());
    }
    Ok(Some(Spec {
        redirects,
        listeners: listeners.into_iter().collect(),
    }))
}

fn udp_conflict(
    text: &str,
    ipv6: bool,
    spec: &Spec,
    owned: &BTreeSet<u64>,
) -> Result<Option<u16>, String> {
    for line in text.lines().skip(1) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let (address, port) = fields
            .get(1)
            .and_then(|s| s.split_once(':'))
            .ok_or("无法识别 UDP 端点")?;
        let port = u16::from_str_radix(port, 16).map_err(|_| "无法识别 UDP 端口")?;
        if !spec
            .redirects
            .iter()
            .any(|r| r.start <= port && port <= r.end)
        {
            continue;
        }
        let ip = if ipv6 {
            if address.len() != 32 {
                return Err("无法识别 IPv6 UDP 地址".into());
            }
            let mut bytes = [0; 16];
            for (part, bytes) in address
                .as_bytes()
                .chunks_exact(8)
                .zip(bytes.chunks_exact_mut(4))
            {
                let word = u32::from_str_radix(
                    std::str::from_utf8(part).map_err(|_| "UDP 地址编码错误")?,
                    16,
                )
                .map_err(|_| "UDP 地址编码错误")?;
                bytes.copy_from_slice(&word.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        } else {
            let word = u32::from_str_radix(address, 16).map_err(|_| "无法识别 IPv4 UDP 地址")?;
            IpAddr::V4(Ipv4Addr::from(word.to_ne_bytes()))
        };
        if ip.is_loopback()
            || matches!(ip, IpAddr::V6(a) if a.to_ipv4_mapped().is_some_and(|v| v.is_loopback()))
        {
            continue;
        }
        let inode = fields
            .get(9)
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("无法识别 UDP socket 身份")?;
        if !owned.contains(&inode) {
            return Ok(Some(port));
        }
    }
    Ok(None)
}

fn check_udp_ownership(state_dir: &Path, spec: &Spec) -> Result<(), String> {
    let mut owned = BTreeSet::new();
    for process in crate::serving_xray_processes_in(state_dir) {
        if !crate::process_is_live(process) {
            return Err("HY2 预检期间受管进程发生变化".into());
        }
        owned.extend(crate::process_socket_inodes(process.pid)?);
        if !crate::process_is_live(process) {
            return Err("HY2 预检期间受管进程身份发生变化".into());
        }
    }
    for (table, ipv6) in [("udp", false), ("udp6", true)] {
        let path = PathBuf::from("/proc/net").join(table);
        let Some(bytes) = fsutil::read_bounded(&path, 16 * 1024 * 1024)? else {
            if ipv6 {
                continue;
            }
            return Err("HY2 预检无法读取主空间 UDP 监听".into());
        };
        let text = std::str::from_utf8(&bytes).map_err(|_| "UDP 监听编码错误")?;
        if let Some(port) = udp_conflict(text, ipv6, spec, &owned)? {
            return Err(format!(
                "HY2 端口段与主空间非受管 UDP socket 冲突（端口 {port}），保留当前服务"
            ));
        }
    }
    Ok(())
}

fn private_route_conflict(destination: &str) -> Result<bool, String> {
    if destination == "default" {
        return Ok(false);
    }
    let (address, prefix) = destination
        .split_once('/')
        .map_or((destination, None), |(a, p)| (a, Some(p)));
    let address: IpAddr = address.parse().map_err(|_| "路由目的地址无法识别")?;
    let bits = if address.is_ipv4() { 32_u32 } else { 128 };
    let prefix = prefix.map_or(Ok(bits), |p| {
        p.parse::<u32>().map_err(|_| "路由前缀无法识别")
    })?;
    if prefix > bits {
        return Err("路由前缀越界".into());
    }
    if prefix < if address.is_ipv4() { 30 } else { 64 } {
        return Ok(false);
    }
    Ok(match address {
        IpAddr::V4(a) => (u32::from(a) ^ u32::from(Ipv4Addr::new(169, 254, 254, 0))) >> 2 == 0,
        IpAddr::V6(a) => {
            (u128::from(a) ^ u128::from(HOST_IP6.parse::<Ipv6Addr>().expect("static IPv6 address")))
                >> 64
                == 0
        }
    })
}

// Check only resources reserved by this realization, not the host's routing
// policy. Broad routes do not reserve this small link; a specific route or an
// existing peer does. Never interpret third-party firewall or QoS rules here.
fn check_private_link(interface: &str) -> Result<(), String> {
    for family in ["-4", "-6"] {
        let routes: Value = serde_json::from_str(&run(
            "ip",
            &["-j", family, "route", "show", "table", "all"],
        )?)
        .map_err(|e| e.to_string())?;
        let routes = routes.as_array().ok_or("无法读取路由表")?;
        if routes.len() > 8192 {
            return Err("HY2 私有链路检查清单超限".into());
        }
        for route in routes {
            if route["dev"] == interface {
                continue;
            }
            if let Some(destination) = route["dst"].as_str() {
                if private_route_conflict(destination)? {
                    return Err("HY2 私有链路与现有具体路由冲突，保留当前服务".into());
                }
            }
        }
        let neighbors: Value = serde_json::from_str(&run("ip", &["-j", family, "neigh", "show"])?)
            .map_err(|e| e.to_string())?;
        let neighbors = neighbors.as_array().ok_or("无法读取邻居清单")?;
        if neighbors.len() > 8192 {
            return Err("HY2 私有链路检查清单超限".into());
        }
        for neighbor in neighbors {
            if neighbor["dev"] == interface {
                continue;
            }
            if let Some(destination) = neighbor["dst"].as_str() {
                if private_route_conflict(destination)? {
                    return Err("HY2 私有链路地址已有外部邻居，保留当前服务".into());
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn preflight(
    state_dir: &Path,
    xray: &str,
    program: &str,
    recovery: bool,
) -> Result<bool, String> {
    let content = hop_content(state_dir, xray)?;
    if hy2_port_hop::parse(&content)?.is_empty() {
        return Ok(false);
    }
    if recovery {
        return Ok(false);
    }
    let version = run(program, &["version"])?;
    if !crate::xray_version_has_capability(&version, CAPABILITY) {
        return Ok(false);
    }
    if crate::xrayupdate::read_upgrade_runtime(state_dir)?.is_some()
        || crate::serving_xray_processes_in(state_dir).len() > 1
    {
        return Err("已有 Xray 双进程运行态，先完成当前操作再迁移 HY2".into());
    }
    let spec = specification(xray, &content)?.ok_or("HY2 网络规格缺失")?;
    timestamp::preflight()?;
    check_udp_ownership(state_dir, &spec)?;
    let reserved_ports = ports::preflight(state_dir, &spec)?;
    let addresses: Value =
        serde_json::from_str(&run("ip", &["-j", "addr", "show"])?).map_err(|e| e.to_string())?;
    verify_root_owner(state_dir)?;
    run("ip", &["-j", "netns", "list"])?;
    let state = read_state(state_dir)?;
    if let Some(state) = &state {
        restore_namespace_handle(state_dir, state)?;
    }
    let (namespace, interface, owner) = identity(state_dir)?;
    check_private_link(&interface)?;
    for link in addresses.as_array().ok_or("无法读取接口地址")? {
        if link["ifname"] == interface {
            continue;
        }
        if link["addr_info"].as_array().is_some_and(|items| {
            items.iter().any(|a| {
                [HOST_IP, PEER_IP, HOST_IP6, PEER_IP6]
                    .iter()
                    .any(|ip| a["local"] == *ip)
            })
        }) {
            return Err("HY2 私有链路地址已被其他接口使用".into());
        }
    }
    let ns_exists = Path::new("/run/netns").join(&namespace).exists();
    if ns_exists
        && state
            .as_ref()
            .is_none_or(|s| boot_id().ok().as_deref() != Some(&s.boot_id))
    {
        return Err("同名 HY2 netns 缺少所有权检查点，拒绝接管".into());
    }
    if let Some(saved) = &state {
        if ns_exists
            && saved.namespace_inode != 0
            && fs::metadata(Path::new("/run/netns").join(&namespace))
                .map_err(|e| e.to_string())?
                .ino()
                != saved.namespace_inode
        {
            return Err("HY2 netns 身份已变化，未停止当前服务".into());
        }
    }
    if run("ip", &["-j", "link", "show"])
        .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()))?
        .as_array()
        .ok_or("无法解析接口清单")?
        .iter()
        .any(|l| l["ifname"] == interface)
        && state
            .as_ref()
            .is_none_or(|s| boot_id().ok().as_deref() != Some(&s.boot_id))
    {
        return Err("同名 HY2 veth 缺少所有权检查点".into());
    }
    // Probe in an anonymous child namespace; a process interruption cannot
    // leave a named preflight namespace that blocks every later attempt.
    let probe_link = format!("bp2{}", &owner[..8]);
    let probe_peer = format!("bq2{}", &owner[..8]);
    let inside = internal_rules(&spec);
    let probe_state = NetworkState {
        version: 1,
        boot_id: boot_id()?,
        owner,
        namespace,
        interface: probe_link.clone(),
        namespace_inode: 0,
        phase: "preparing".into(),
        spec,
        rules_sha256: String::new(),
    };
    let outside = root_rules(&probe_state);
    let script = format!("set -eu\nip link add {probe_link} alias {} type veth peer name {probe_peer}\nsysctl -qw net.ipv6.conf.{probe_link}.disable_ipv6=0 net.ipv4.ip_local_reserved_ports={reserved_ports}\nip -6 addr add {HOST_IP6}/64 dev {probe_link} nodad\nnft -f - <<'HY2_INTERNAL'\n{inside}\nHY2_INTERNAL\nsysctl -qw net.netfilter.nf_conntrack_udp_timeout=30 net.netfilter.nf_conntrack_udp_timeout_stream=60\nnft -c -f - <<'HY2_EXTERNAL'\n{outside}\nHY2_EXTERNAL\n", probe_state.owner);
    crate::command::run_shell_in_private_network(&script, Duration::from_secs(5))?;
    Ok(true)
}

fn root_rules(state: &NetworkState) -> String {
    let mut script = String::new();
    for (family, ip, peer) in [("ip", "ip", PEER_IP), ("ip6", "ip6", PEER_IP6)] {
        script.push_str(&format!("add table {family} {TABLE}\ndelete table {family} {TABLE}\ntable {family} {TABLE} {{\n chain pre {{ type filter hook prerouting priority -310;\n"));
        if family == "ip" {
            script.push_str(&format!("iifname != \"{}\" fib daddr type local ip protocol udp ip frag-off & 0x1fff != 0 counter dup to {peer} device \"{}\"\n", state.interface, state.interface));
            script.push_str(&format!("iifname != \"{}\" fib daddr type local icmp type {{ destination-unreachable, time-exceeded, parameter-problem }} counter dup to {peer} device \"{}\"\n", state.interface, state.interface));
        } else {
            script.push_str(&format!("iifname != \"{}\" fib daddr type local exthdr frag exists meta l4proto udp frag frag-off != 0 counter dup to {peer} device \"{}\"\n", state.interface, state.interface));
            script.push_str(&format!("iifname != \"{}\" fib daddr type local icmpv6 type {{ destination-unreachable, packet-too-big, time-exceeded, parameter-problem }} counter dup to {peer} device \"{}\"\n", state.interface, state.interface));
        }
        for range in &state.spec.redirects {
            // A local probe reaches this node's public address through lo.
            // Preserve that path, while loopback addresses stay in their own
            // namespace and must never be forwarded to the private link.
            let public_source = if family == "ip" {
                "ip saddr != 127.0.0.0/8 ip daddr != 127.0.0.0/8"
            } else {
                "ip6 saddr != ::1 ip6 daddr != ::1"
            };
            script.push_str(&format!("iifname != \"{}\" {public_source} fib daddr type local udp dport {}-{} counter dup to {peer} device \"{}\" drop\n", state.interface, range.start, range.end, state.interface));
        }
        script.push_str(&format!(
            "iifname \"{}\" meta l4proto udp counter dup to {ip} daddr drop\n",
            state.interface
        ));
        if family == "ip" {
            script.push_str(&format!(
                "iifname \"{}\" ip protocol icmp counter dup to ip daddr drop\n",
                state.interface
            ));
        } else {
            // Keep neighbor discovery on its original link and interface scope.
            script.push_str(&format!("iifname \"{}\" icmpv6 type {{ destination-unreachable, packet-too-big, time-exceeded, parameter-problem }} counter dup to ip6 daddr drop\n", state.interface));
        }
        script.push_str(" }\n chain output_raw { type filter hook output priority -310;\n");
        for range in &state.spec.redirects {
            let addresses = if family == "ip" {
                "ip saddr != 127.0.0.0/8 ip daddr != 127.0.0.0/8"
            } else {
                "ip6 saddr != ::1 ip6 daddr != ::1"
            };
            // Local public-address probes pass OUTPUT before the loopback
            // PREROUTING diversion. Other NAT/stateful users may have enabled
            // conntrack here, so scope NOTRACK to exactly this local HY2 path.
            script.push_str(&format!(
                "{addresses} fib daddr type local udp dport {}-{} counter notrack\n",
                range.start, range.end
            ));
        }
        script.push_str(&format!(
            " }}\n chain owner_{} {{\n }}\n}}\n",
            &state.owner[..12]
        ));
    }
    // Removing the owned legacy NAT hooks is part of the same transaction.
    script
        .push_str("add table inet brocade_hy2_port_hop\ndelete table inet brocade_hy2_port_hop\n");
    script
}

fn internal_rules(spec: &Spec) -> String {
    let mut text = "add table inet brocade_hy2_nat\ndelete table inet brocade_hy2_nat\ntable inet brocade_hy2_nat { chain pre { type nat hook prerouting priority dstnat;\n".to_owned();
    for range in &spec.redirects {
        text.push_str(&format!(
            "udp dport {}-{} counter redirect to :{}\n",
            range.start, range.end, range.to
        ));
    }
    text.push_str("}\n}\n");
    text
}

fn rules_digest(state: &NetworkState) -> Result<String, String> {
    fn clean(value: &mut Value) {
        match value {
            Value::Array(values) => values.iter_mut().for_each(clean),
            Value::Object(object) => {
                for key in ["handle", "packets", "bytes"] {
                    object.remove(key);
                }
                object.values_mut().for_each(clean);
            }
            _ => (),
        }
    }
    let mut tables = Vec::new();
    for output in [
        run("nft", &["-j", "list", "table", "ip", TABLE])?,
        run("nft", &["-j", "list", "table", "ip6", TABLE])?,
        net(
            &state.namespace,
            "nft",
            &["-j", "list", "table", "inet", "brocade_hy2_nat"],
        )?,
    ] {
        let value: Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
        let objects = value
            .get("nftables")
            .and_then(Value::as_array)
            .ok_or("nft JSON 不完整")?;
        let mut objects = Value::Array(
            objects
                .iter()
                .filter(|o| o.get("metainfo").is_none())
                .cloned()
                .collect(),
        );
        clean(&mut objects);
        tables.push(objects);
    }
    Ok(sha256_hex(
        &serde_json::to_vec(&tables).map_err(|e| e.to_string())?,
    ))
}

fn install_internal(state: &NetworkState) -> Result<(), String> {
    let script = internal_rules(&state.spec);
    crate::run_shell_with_timeout(
        &format!(
            "set -eu\nip netns exec {} nft -f - <<'BROCADE_NFT'\n{script}\nBROCADE_NFT\n",
            state.namespace
        ),
        Duration::from_secs(5),
    )
    .map(|_| ())
}

fn verify_root_owner(state_dir: &Path) -> Result<(), String> {
    let (_, _, owner) = identity(state_dir)?;
    let marker = format!("owner_{}", &owner[..12]);
    let tables: Value =
        serde_json::from_str(&run("nft", &["-j", "list", "tables"])?).map_err(|e| e.to_string())?;
    let tables = tables
        .get("nftables")
        .and_then(Value::as_array)
        .ok_or("无法读取 nft 表清单")?;
    for family in ["ip", "ip6"] {
        if !tables
            .iter()
            .any(|t| t["table"]["family"] == family && t["table"]["name"] == TABLE)
        {
            continue;
        }
        let table: Value =
            serde_json::from_str(&run("nft", &["-j", "list", "table", family, TABLE])?)
                .map_err(|e| e.to_string())?;
        if !table
            .get("nftables")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|o| o["chain"]["name"] == marker))
        {
            return Err("HY2 转发表缺少匹配的所有权标记，拒绝覆盖".into());
        }
    }
    Ok(())
}

fn remove_root(state_dir: &Path) -> Result<(), String> {
    verify_root_owner(state_dir)?;
    nft(&format!("add table ip {TABLE}\ndelete table ip {TABLE}\nadd table ip6 {TABLE}\ndelete table ip6 {TABLE}\n"))
}

fn remove_owned(state_dir: &Path) -> Result<(), String> {
    let Some(state) = read_state(state_dir)? else {
        return Ok(());
    };
    if state.boot_id != boot_id()? {
        return Ok(());
    }
    restore_namespace_handle(state_dir, &state)?;
    let path = Path::new("/run/netns").join(&state.namespace);
    if path.exists() {
        let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
        if state.namespace_inode != 0 && meta.ino() != state.namespace_inode {
            return Err("HY2 netns 身份变化，拒绝删除".into());
        }
        let waiting = std::time::Instant::now();
        while !run("ip", &["netns", "pids", &state.namespace])?
            .trim()
            .is_empty()
        {
            if waiting.elapsed() >= Duration::from_secs(5) {
                return Err("HY2 netns 仍有进程，拒绝删除".into());
            }
            // A count observer may already have entered before the checkpoint
            // stopped new observations. Never signal unknown namespace users.
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    // Keep the named namespace pinned until its owned veth has been removed.
    // Unmounting first lets namespace GC asynchronously delete the peer while
    // we are between the host link dump and RTM_DELLINK.
    let links: Value =
        serde_json::from_str(&run("ip", &["-j", "link", "show"])?).map_err(|e| e.to_string())?;
    if let Some(link) = links
        .as_array()
        .and_then(|a| a.iter().find(|l| l["ifname"] == state.interface))
    {
        if link["ifalias"] != state.owner {
            return Err("HY2 veth 所有权变化，拒绝删除".into());
        }
        if let Err(error) = run("ip", &["link", "delete", &state.interface]) {
            // A prior interrupted cleanup may already have unmounted the netns.
            // Accept an asynchronously vanished link only after a fresh dump;
            // retain every other error rather than matching localized stderr.
            let remaining: Value = serde_json::from_str(&run("ip", &["-j", "link", "show"])?)
                .map_err(|e| e.to_string())?;
            if remaining
                .as_array()
                .ok_or("HY2 清理后的接口清单不完整")?
                .iter()
                .any(|link| link["ifname"] == state.interface)
            {
                return Err(error);
            }
        }
    }
    if path.exists() {
        run("ip", &["netns", "delete", &state.namespace])?;
    }
    Ok(())
}

/// Called only after the managed Xray has stopped, under the process/accounting lock.
pub(crate) fn prepare_start(
    state_dir: &Path,
    content: &str,
    isolate: bool,
) -> Result<Option<PathBuf>, String> {
    let artifact = hop_content(state_dir, content)?;
    let spec = if isolate {
        specification(content, &artifact)?
    } else {
        None
    };
    let has_ranges = !hy2_port_hop::parse(&artifact)?.is_empty();
    if let Some(spec) = &spec {
        check_udp_ownership(state_dir, spec)?;
    }
    if let Some(mut previous) = read_state(state_dir)? {
        previous.phase = "preparing".into();
        write_json(state_dir, STATE, &previous)?;
        remove_root(state_dir)?;
        remove_owned(state_dir)?;
        ports::release(state_dir)?;
    }
    if !isolate || spec.is_none() {
        if state_dir.join(STATE).exists() {
            fs::remove_file(state_dir.join(STATE)).map_err(|e| e.to_string())?;
        }
        if state_dir.join(POLICY).exists() {
            fs::remove_file(state_dir.join(POLICY)).map_err(|e| e.to_string())?;
        }
        if has_ranges {
            hy2_port_hop::apply_content(&artifact)?;
        } else {
            hy2_port_hop::disable()?;
        }
        return Ok(None);
    }
    let spec = spec.ok_or("HY2 网络计划缺失")?;
    let (namespace, interface, owner) = identity(state_dir)?;
    let mut state = NetworkState {
        version: 1,
        boot_id: boot_id()?,
        namespace,
        interface,
        owner,
        namespace_inode: 0,
        phase: "preparing".into(),
        spec,
        rules_sha256: String::new(),
    };
    write_json(state_dir, STATE, &state)?;
    ports::reserve(state_dir, &state.spec)?;
    run("ip", &["netns", "add", &state.namespace])?;
    let meta =
        fs::metadata(format!("/run/netns/{}", state.namespace)).map_err(|e| e.to_string())?;
    state.namespace_inode = meta.ino();
    write_json(state_dir, STATE, &state)?;
    let peer = format!("{}p", state.interface);
    // Assign ownership before exposing either end in the main namespace.
    // Some kernels ignore IFLA_IFALIAS during RTM_NEWLINK; setting it only
    // after a main-namespace creation leaves an unowned link after a crash.
    net(
        &state.namespace,
        "ip",
        &[
            "link",
            "add",
            &state.interface,
            "type",
            "veth",
            "peer",
            "name",
            &peer,
        ],
    )?;
    net(
        &state.namespace,
        "ip",
        &["link", "set", &state.interface, "alias", &state.owner],
    )?;
    net(
        &state.namespace,
        "ip",
        &[
            "link",
            "set",
            &state.interface,
            "netns",
            &std::process::id().to_string(),
        ],
    )?;
    // Only the owned transport link needs IPv6. Host defaults may deliberately
    // disable IPv6 on public interfaces; do not rewrite those or the all/default
    // switches. The anonymous preflight exercises this same per-link operation.
    run(
        "sysctl",
        &[
            "-qw",
            &format!("net.ipv6.conf.{}.disable_ipv6=0", state.interface),
        ],
    )?;
    run(
        "ip",
        &[
            "addr",
            "add",
            &format!("{HOST_IP}/30"),
            "dev",
            &state.interface,
        ],
    )?;
    run(
        "ip",
        &[
            "-6",
            "addr",
            "add",
            &format!("{HOST_IP6}/64"),
            "dev",
            &state.interface,
            "nodad",
        ],
    )?;
    run(
        "ip",
        // Do not add an artificial 1280-byte bottleneck: some QUIC Initials
        // are 1250-byte UDP payloads even on IPv6. Real path MTU is learned
        // through ICMP errors forwarded back to this socket's namespace.
        &["link", "set", &state.interface, "mtu", "65535", "up"],
    )?;
    net(&state.namespace, "ip", &["link", "set", "lo", "up"])?;
    net(
        &state.namespace,
        "sysctl",
        &["-qw", &format!("net.ipv6.conf.{peer}.disable_ipv6=0")],
    )?;
    net(
        &state.namespace,
        "ip",
        &["addr", "add", &format!("{PEER_IP}/30"), "dev", &peer],
    )?;
    net(
        &state.namespace,
        "ip",
        &[
            "-6",
            "addr",
            "add",
            &format!("{PEER_IP6}/64"),
            "dev",
            &peer,
            "nodad",
        ],
    )?;
    net(
        &state.namespace,
        "ip",
        &["link", "set", &peer, "mtu", "65535", "up"],
    )?;
    net(
        &state.namespace,
        "ip",
        &["route", "add", "default", "via", HOST_IP],
    )?;
    net(
        &state.namespace,
        "ip",
        &["-6", "route", "add", "default", "via", HOST_IP6],
    )?;
    install_internal(&state)?;
    timestamp::ensure(&state.interface)?;
    net(
        &state.namespace,
        "sysctl",
        &[
            "-qw",
            "net.netfilter.nf_conntrack_udp_timeout=30",
            "net.netfilter.nf_conntrack_udp_timeout_stream=60",
        ],
    )?;
    let policy = SocketPolicy {
        version: 1,
        namespace_path: format!("/run/netns/{}", state.namespace),
        namespace_inode: meta.ino(),
        namespace_device: meta.dev(),
        boot_id: &state.boot_id,
        listeners: &state.spec.listeners,
    };
    write_json(state_dir, POLICY, &policy)?;
    state.phase = "ready".into();
    write_json(state_dir, STATE, &state)?;
    Ok(Some(
        fs::canonicalize(state_dir.join(POLICY)).map_err(|e| e.to_string())?,
    ))
}

pub(crate) fn listener_change(
    state_dir: &Path,
    xray: &DesiredArtifact,
    hops: &DesiredArtifact,
) -> Result<bool, String> {
    let content = match hops {
        DesiredArtifact::Present { content, .. } => content.as_str(),
        DesiredArtifact::Disabled { .. } => "{}",
        DesiredArtifact::Unmanaged { .. } => return Ok(false),
    };
    let state = read_state(state_dir)?;
    if state.is_none() && hy2_port_hop::parse(content)?.is_empty() {
        return Ok(false);
    }
    let current;
    let xray = match xray {
        DesiredArtifact::Present { content, .. } => content.as_str(),
        DesiredArtifact::Unmanaged { .. } => {
            current = fs::read_to_string(state_dir.join("xray.json")).map_err(|e| e.to_string())?;
            current.as_str()
        }
        DesiredArtifact::Disabled { .. } => return Ok(false),
    };
    if let Some(state) = state {
        return Ok(
            specification(xray, content)?.is_none_or(|s| s.listeners != state.spec.listeners)
        );
    }
    if hy2_port_hop::parse(content)?.is_empty() {
        return Ok(false);
    }
    let version = run(&crate::xray_program(), &["version"])?;
    let supported = crate::xray_version_has_capability(&version, CAPABILITY);
    if supported {
        specification(xray, content)?;
    }
    Ok(supported)
}

pub(crate) fn activate(state_dir: &Path) -> Result<(), String> {
    let Some(mut state) = read_state(state_dir)? else {
        return Ok(());
    };
    if state.phase != "ready" && state.phase != "active" {
        return Err("HY2 网络尚未准备完成".into());
    }
    verify_root_owner(state_dir)?;
    check_udp_ownership(state_dir, &state.spec)?;
    timestamp::ensure(&state.interface)?;
    ports::reserve(state_dir, &state.spec)?;
    nft(&root_rules(&state))?;
    state.rules_sha256 = rules_digest(&state)?;
    state.phase = "active".into();
    write_json(state_dir, STATE, &state)
}

pub(crate) fn cleanup(state_dir: &Path) -> Result<(), String> {
    let Some(mut state) = read_state(state_dir)? else {
        return discard_pending(state_dir);
    };
    state.phase = "preparing".into();
    write_json(state_dir, STATE, &state)?;
    remove_root(state_dir)?;
    remove_owned(state_dir)?;
    ports::release(state_dir)?;
    for name in [STATE, POLICY, PENDING] {
        match fs::remove_file(state_dir.join(name)) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

pub(crate) fn discard_pending(state_dir: &Path) -> Result<(), String> {
    match fs::remove_file(state_dir.join(PENDING)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn effective_hops(state_dir: &Path) -> Result<String, String> {
    let content = fs::read_to_string(state_dir.join("xray.json")).map_err(|e| e.to_string())?;
    hop_content(state_dir, &content)
}

/// Complete the small cross-file commit after a healthy local recovery. An
/// interrupted deployment must not reinstall its previous port ranges forever.
pub(crate) fn finish_pending(state_dir: &Path) -> Result<(), String> {
    let Some(text) = optional_file(&state_dir.join(PENDING))? else {
        return Ok(());
    };
    let pending: Pending = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let current = fs::read(state_dir.join("xray.json")).map_err(|e| e.to_string())?;
    if sha256_hex(&current) != pending.xray_sha256 {
        return Ok(());
    }
    let ranges = hy2_port_hop::parse(&pending.content)?;
    let (keep, remove, content) = if ranges.is_empty() {
        (
            "hy2_port_hop.disabled",
            "hy2_port_hop.json",
            "HY2 hopping disabled",
        )
    } else {
        (
            "hy2_port_hop.json",
            "hy2_port_hop.disabled",
            pending.content.as_str(),
        )
    };
    fsutil::atomic_write_private(&state_dir.join(keep), content.as_bytes())?;
    match fs::remove_file(state_dir.join(remove)) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.to_string()),
    }
    fs::remove_file(state_dir.join(PENDING)).map_err(|e| e.to_string())
}

pub(crate) fn reconcile(state_dir: &Path) -> Result<(), String> {
    let Some(mut state) = read_state(state_dir)? else {
        return Ok(());
    };
    if state.boot_id != boot_id()? {
        return Ok(());
    }
    if state.phase == "active" {
        restore_namespace_handle(state_dir, &state)?;
        ports::reserve(state_dir, &state.spec)?;
        let meta = fs::metadata(format!("/run/netns/{}", state.namespace))
            .map_err(|e| format!("HY2 netns 缺失：{e}"))?;
        if meta.ino() != state.namespace_inode {
            return Err("HY2 netns 身份变化".into());
        }
        timestamp::ensure(&state.interface)?;
        let config = fs::read_to_string(state_dir.join("xray.json")).map_err(|e| e.to_string())?;
        let wanted = specification(&config, &hop_content(state_dir, &config)?)?
            .ok_or("HY2 当前映射已移除，需要重启恢复")?;
        let matched = wanted == state.spec
            && rules_digest(&state).ok().as_deref() == Some(&state.rules_sha256);
        if !matched {
            state.spec = wanted;
            check_udp_ownership(state_dir, &state.spec)?;
            ports::reserve(state_dir, &state.spec)?;
            verify_root_owner(state_dir)?;
            install_internal(&state)?;
            nft(&root_rules(&state))?;
            state.rules_sha256 = rules_digest(&state)?;
            write_json(state_dir, STATE, &state)?;
        }
    }
    Ok(())
}

pub(crate) fn apply(state_dir: &Path, desired: &DesiredArtifact) -> Result<(), String> {
    match desired {
        DesiredArtifact::Present { content, .. } if active(state_dir) => {
            let config =
                fs::read_to_string(state_dir.join("xray.json")).map_err(|e| e.to_string())?;
            let spec = specification(&config, content)?.ok_or("活动 HY2 网络不能应用空映射")?;
            let mut state = read_state(state_dir)?.ok_or("HY2 检查点缺失")?;
            restore_namespace_handle(state_dir, &state)?;
            if state.spec.listeners != spec.listeners {
                return Err("HY2 listener 改变需要重启 Xray".into());
            }
            state.spec = spec;
            check_udp_ownership(state_dir, &state.spec)?;
            timestamp::ensure(&state.interface)?;
            ports::reserve(state_dir, &state.spec)?;
            verify_root_owner(state_dir)?;
            install_internal(&state)?;
            nft(&root_rules(&state))?;
            state.rules_sha256 = rules_digest(&state)?;
            write_json(state_dir, STATE, &state)?;
        }
        DesiredArtifact::Disabled { .. } => {
            cleanup(state_dir)?;
            hy2_port_hop::disable()?;
        }
        DesiredArtifact::Unmanaged { .. } => return Ok(()),
        _ => hy2_port_hop::apply(desired)?,
    }
    match fs::remove_file(state_dir.join(PENDING)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn matches(state_dir: &Path, desired: &DesiredArtifact) -> bool {
    if let DesiredArtifact::Present { content, .. } = desired {
        if active(state_dir) {
            return (|| -> Result<bool, String> {
                let state = read_state(state_dir)?.ok_or("HY2 状态缺失")?;
                let config =
                    fs::read_to_string(state_dir.join("xray.json")).map_err(|e| e.to_string())?;
                let wanted = specification(&config, content)?;
                if wanted.as_ref() != Some(&state.spec) {
                    return Ok(false);
                }
                Ok(timestamp::matches(&state.interface)?
                    && !state.rules_sha256.is_empty()
                    && rules_digest(&state)? == state.rules_sha256)
            })()
            .unwrap_or(false);
        }
    }
    hy2_port_hop::matches(desired)
}

pub(crate) fn observe(state_dir: &Path) -> (Option<bool>, Option<u64>) {
    let state = match read_state(state_dir) {
        Ok(Some(state)) => state,
        Ok(None) => return (Some(false), None),
        Err(_) => return (None, None),
    };
    if state.phase != "active" || boot_id().ok().as_deref() != Some(&state.boot_id) {
        return (Some(false), None);
    }
    let path = format!("/run/netns/{}", state.namespace);
    let read = (|| -> Result<u64, String> {
        let before = fs::metadata(&path).map_err(|e| e.to_string())?;
        if before.ino() != state.namespace_inode {
            return Err("HY2 namespace changed".into());
        }
        let value = net(
            &state.namespace,
            "cat",
            &["/proc/sys/net/netfilter/nf_conntrack_count"],
        )?;
        let after = fs::metadata(&path).map_err(|e| e.to_string())?;
        if before.ino() != after.ino() {
            return Err("HY2 namespace changed during read".into());
        }
        value
            .trim()
            .parse()
            .map_err(|e| format!("HY2 conntrack count: {e}"))
    })();
    (Some(true), read.ok())
}
