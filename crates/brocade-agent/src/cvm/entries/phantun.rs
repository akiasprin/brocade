//! Per-entry Phantun workers share the existing binaries and CLI, with independent ownership.
use super::*;
use std::{
    collections::BTreeMap,
    io,
    net::{Ipv6Addr, SocketAddr, UdpSocket},
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    time::Instant,
};
const TABLE: &str = "brocade_entry_phantun";
struct Worker {
    fingerprint: String,
    child: Child,
    endpoint: SocketAddr,
    received_since_start: bool,
}
impl Worker {
    fn stop(&mut self) -> Result<()> {
        if self
            .child
            .try_wait()
            .map_err(|_| "cannot inspect entry Phantun cleanup")?
            .is_some()
        {
            return Ok(());
        }
        self.child
            .kill()
            .map_err(|_| "cannot stop owned entry Phantun process")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self
                .child
                .try_wait()
                .map_err(|_| "cannot reap entry Phantun process")?
                .is_some()
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err("entry Phantun cleanup is unconfirmed".into())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

static WORKERS: OnceLock<Mutex<BTreeMap<String, Worker>>> = OnceLock::new();
fn device(t: &EntryTunnel) -> String {
    format!(
        "bep{}",
        &brocade_core::hash::sha256_hex(t.id.as_bytes())[..12]
    )
}
fn addresses(t: &EntryTunnel) -> (Ipv4Addr, Ipv4Addr, Ipv6Addr, Ipv6Addr) {
    let side = u32::from(t.side == EntryTunnelSide::Target);
    let base = u32::from(Ipv4Addr::new(240, 4, 0, 0)) + u32::from(t.slot) * 8 + side * 4;
    let v6 = |host| Ipv6Addr::new(0xfd42, 0x6263, 0x656e, t.slot, side as u16, 0, 0, host);
    (
        Ipv4Addr::from(base + 1),
        Ipv4Addr::from(base + 2),
        v6(1),
        v6(2),
    )
}
pub(super) struct Adapter {
    pub(super) endpoint: SocketAddr,
    pub(super) restarted: bool,
}
pub(super) fn ensure(root: &Path, t: &EntryTunnel, wg_port: Option<u16>) -> Result<Adapter> {
    let fingerprint = brocade_core::hash::sha256_hex(
        &serde_json::to_vec(&(t, wg_port)).map_err(|_| "invalid entry Phantun identity")?,
    );
    let mut workers = WORKERS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "entry Phantun worker registry unavailable")?;
    if let Some(worker) = workers.get_mut(&t.id) {
        if worker.fingerprint == fingerprint
            && worker
                .child
                .try_wait()
                .map_err(|_| "cannot inspect entry Phantun process")?
                .is_none()
        {
            return Ok(Adapter {
                endpoint: worker.endpoint,
                restarted: false,
            });
        }
    }
    if let Some(worker) = workers.get_mut(&t.id) {
        worker.stop()?;
    }
    workers.remove(&t.id);
    if workers.len() >= MAX_ENTRY_TUNNELS {
        return Err("entry Phantun worker capacity exceeded".into());
    }
    let dev = device(t);
    if tunnels::inventory()?
        .iter()
        .any(|link| link["ifname"] == dev)
    {
        return Err("入口 Phantun 设备已存在，未接管其他进程的网卡".into());
    }
    let (local, peer, local6, peer6) = addresses(t);
    let udp = if t.listens() {
        wg_port
            .filter(|p| *p != 0)
            .ok_or("entry Phantun server requires a WireGuard socket")?
    } else {
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| "cannot allocate entry Phantun UDP adapter")?
            .local_addr()
            .map_err(|_| "cannot inspect entry Phantun UDP adapter")?
            .port()
    };
    let endpoint = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), udp);
    let instance = if t.listens() {
        crate::phantun::PhantunInstance::server(
            t.port,
            udp,
            dev.clone(),
            local.to_string(),
            peer.to_string(),
        )
    } else {
        crate::phantun::PhantunInstance::client(
            udp,
            SocketAddr::new(t.offer.endpoint, t.port).to_string(),
            dev.clone(),
            local.to_string(),
            peer.to_string(),
        )
    };
    let mut args = instance.args();
    if t.offer.endpoint.is_ipv4() {
        args.push("--ipv4-only".into());
    } else {
        args.extend([
            "--tun-local6".into(),
            local6.to_string(),
            "--tun-peer6".into(),
            peer6.to_string(),
        ]);
    }
    let mut command = Command::new(instance.program());
    command
        .args(args)
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    super::super::diagnostics::capture(
        &mut command,
        root,
        &t.id,
        NetworkLogSource::Phantun,
        super::super::diagnostics::secrets(&[t.credential.clone(), t.peer_credential.clone()]),
    );
    // SAFETY: only async-signal-safe syscalls execute after fork; children cannot outlive Agent.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() == 1 {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Agent exited before Phantun startup",
                ));
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|_| "cannot start entry Phantun process")?;
    let mut worker = Worker {
        fingerprint,
        child,
        endpoint,
        received_since_start: false,
    };
    let started = Instant::now();
    loop {
        if worker
            .child
            .try_wait()
            .map_err(|_| "cannot inspect entry Phantun startup")?
            .is_some()
        {
            return Err("入口 Phantun 启动失败，请检查运行包和网络权限".into());
        }
        if tunnels::inventory()?
            .iter()
            .any(|link| link["ifname"] == dev)
        {
            run_command(
                "ip",
                &[
                    "link",
                    "set",
                    "dev",
                    &dev,
                    "alias",
                    &format!("brocade-entry-phantun:{}", t.id),
                ],
            )
            .map_err(|_| "cannot mark owned entry Phantun interface")?;
            break;
        }
        if started.elapsed() > Duration::from_secs(3) {
            return Err("entry Phantun interface startup timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    workers.insert(t.id.clone(), worker);
    Ok(Adapter {
        endpoint,
        restarted: true,
    })
}

fn received(t: &EntryTunnel) -> Result<u64> {
    let raw = run_command("wg", &["show", &plan::device(t), "transfer"]).map_err(|error| {
        super::super::diagnostics::wireguard_error("WireGuard 入口接收流量读取", &error)
    })?;
    raw.lines().try_fold(0u64, |total, line| {
        let bytes = line
            .split_whitespace()
            .nth(1)
            .ok_or("missing WireGuard receive counter")?
            .parse::<u64>()
            .map_err(|_| "invalid WireGuard receive counter")?;
        total
            .checked_add(bytes)
            .ok_or_else(|| "WireGuard receive counter overflow".into())
    })
}

/// A cached WG handshake can outlive a dead Phantun process. Require authenticated
/// receive traffic after this process starts before reporting the new path connected.
pub(super) fn connected(t: &EntryTunnel) -> Result<bool> {
    let Some(workers) = WORKERS.get() else {
        return Ok(false);
    };
    let mut workers = workers
        .lock()
        .map_err(|_| "entry Phantun registry unavailable")?;
    let Some(worker) = workers.get_mut(&t.id) else {
        return Ok(false);
    };
    if worker
        .child
        .try_wait()
        .map_err(|_| "cannot inspect entry Phantun health")?
        .is_some()
    {
        return Ok(false);
    }
    worker.received_since_start |= received(t)? > 0;
    Ok(worker.received_since_start)
}
pub(super) fn retain(desired: &EntryDesired) -> Result<()> {
    if let Some(workers) = WORKERS.get() {
        let mut workers = workers
            .lock()
            .map_err(|_| "entry Phantun registry unavailable")?;
        let removed = workers
            .keys()
            .filter(|id| {
                !desired.tunnels.iter().any(|t| {
                    &t.id == *id && t.wireguard_transport == EntryWireguardTransport::Phantun
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        for id in removed {
            if let Some(worker) = workers.get_mut(&id) {
                worker.stop()?;
            }
            workers.remove(&id);
        }
    }
    // Recover owned nonpersistent devices left during an interrupted startup.
    for link in tunnels::inventory()? {
        let Some(id) = link["ifalias"]
            .as_str()
            .and_then(|s| s.strip_prefix("brocade-entry-phantun:"))
        else {
            continue;
        };
        if !desired
            .tunnels
            .iter()
            .any(|t| t.id == id && t.wireguard_transport == EntryWireguardTransport::Phantun)
        {
            let name = link["ifname"]
                .as_str()
                .ok_or("invalid owned Phantun interface")?;
            run_command("ip", &["link", "delete", "dev", name])
                .map_err(|_| "cannot remove withdrawn Phantun interface")?;
        }
    }
    Ok(())
}
fn ruleset(desired: &EntryDesired) -> String {
    if !desired
        .tunnels
        .iter()
        .any(|t| t.wireguard_transport == EntryWireguardTransport::Phantun)
    {
        return format!("add table inet {TABLE}\ndelete table inet {TABLE}\n");
    }
    let mut script=format!("add table inet {TABLE}\ndelete table inet {TABLE}\ntable inet {TABLE} {{\n chain incoming {{ type nat hook prerouting priority -100; }}\n chain outgoing {{ type nat hook postrouting priority 100; }}\n chain forward {{ type filter hook forward priority -4; policy accept; }}\n}}\n");
    for t in desired
        .tunnels
        .iter()
        .filter(|t| t.wireguard_transport == EntryWireguardTransport::Phantun)
    {
        let dev = device(t);
        let (_, peer, _, peer6) = addresses(t);
        if t.listens() {
            let (family, address) = if t.offer.endpoint.is_ipv4() {
                ("ip", peer.to_string())
            } else {
                ("ip6", peer6.to_string())
            };
            script.push_str(&format!("add rule inet {TABLE} incoming iifname != \"bep*\" fib daddr type local tcp dport {} dnat {family} to {address}\n",t.port));
        }
        script.push_str(&format!(
            "add rule inet {TABLE} outgoing iifname \"{dev}\" ip saddr {peer} masquerade\n"
        ));
        if t.offer.endpoint.is_ipv6() {
            script.push_str(&format!(
                "add rule inet {TABLE} outgoing iifname \"{dev}\" ip6 saddr {peer6} masquerade\n"
            ));
        }
        script.push_str(&format!("add rule inet {TABLE} forward iifname \"{dev}\" accept\nadd rule inet {TABLE} forward oifname \"{dev}\" accept\n"));
    }
    script
}

pub(super) fn apply_rules(root: &Path, desired: &EntryDesired) -> Result<()> {
    let path = root.join("cvm-entries/phantun.nft");
    atomic_write_private(&path, ruleset(desired).as_bytes())?;
    let file = path.to_str().ok_or("invalid entry Phantun rules path")?;
    run_command("nft", &["-c", "-f", file]).map_err(|error| {
        super::super::diagnostics::command_error(
            root,
            None,
            "Phantun 防火墙规则校验失败",
            &error,
            &[],
        )
    })?;
    run_command("nft", &["-f", file]).map_err(|error| {
        super::super::diagnostics::command_error(
            root,
            None,
            "Phantun 防火墙规则安装失败",
            &error,
            &[],
        )
    })?;
    docker_rules(root, desired)?;
    Ok(())
}

fn docker_rules(root: &Path, desired: &EntryDesired) -> Result<()> {
    for family in ["ip", "ip6"] {
        let output = capture_command_with_timeout(
            "nft",
            &["-j", "list", "chain", family, "filter", "DOCKER-USER"],
            Duration::from_secs(5),
        )
        .map_err(|_| "cannot inspect Docker forwarding extension")?;
        if !output.status.success() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| "invalid Docker forwarding inventory")?;
        let mut rules = String::new();
        for item in value["nftables"]
            .as_array()
            .ok_or("missing Docker forwarding inventory")?
        {
            let rule = &item["rule"];
            if rule["comment"] == "brocade-entry-phantun" {
                let handle = rule["handle"]
                    .as_u64()
                    .ok_or("invalid owned Docker rule handle")?;
                rules.push_str(&format!(
                    "delete rule {family} filter DOCKER-USER handle {handle}\n"
                ));
            }
        }
        if desired
            .tunnels
            .iter()
            .any(|t| t.wireguard_transport == EntryWireguardTransport::Phantun)
        {
            rules.push_str(&format!("insert rule {family} filter DOCKER-USER iifname \"bep*\" accept comment \"brocade-entry-phantun\"\ninsert rule {family} filter DOCKER-USER oifname \"bep*\" accept comment \"brocade-entry-phantun\"\n"));
        }
        if !rules.is_empty() {
            let path = root.join(format!("cvm-entries/phantun-docker-{family}.nft"));
            atomic_write_private(&path, rules.as_bytes())?;
            run_command(
                "nft",
                &["-f", path.to_str().ok_or("invalid Docker forwarding path")?],
            )
            .map_err(|_| "cannot update owned Docker forwarding rules")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod ruleset_tests {
    use super::*;

    #[test]
    fn desired_without_phantun_removes_the_owned_table() {
        assert_eq!(
            ruleset(&EntryDesired::default()),
            "add table inet brocade_entry_phantun\ndelete table inet brocade_entry_phantun\n"
        );
    }
}
