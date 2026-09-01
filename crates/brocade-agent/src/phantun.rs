//! The fake-TCP layer: starting and stopping phantun processes, TUN devices, and
//! the nftables rules that steer inbound TCP onto the TUN.
//!
//! It sits underneath wg — for a fake-TCP peer in `wg0.conf`, Endpoint points at
//! the phantun client's local loopback port, so convergence runs phantun first
//! and wg second (see `converge_linux` in main.rs).
use std::{collections::BTreeSet, fs, path::Path, time::Duration};

use brocade_deployment::{
    plan::{AppliedArtifactState, DesiredArtifact},
    protocol::{BinarySource, PhantunBinaries},
};

use crate::{
    artifact_dirty, command_success, present_file_state, run_shell, run_shell_with_timeout,
    shell_quote, write_private,
};

pub(crate) const PHANTUN_BOUNDED_LOG_MARKER: &str = "phantun.bounded-log-v2";
const PHANTUN_OLD_BOUNDED_LOG_MARKER: &str = "phantun.bounded-log-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
enum PhantunInstanceKind {
    Server {
        tcp_port: u16,
        forward_to_udp_port: u16,
    },
    Client {
        peer: String,
        listen_udp_port: u16,
        remote_tcp_endpoint: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhantunTun {
    name: String,
    local: String,
    peer: String,
}

/// One exact process from `phantun.json`.
///
/// Keeping the command line beside the health description matters for servers:
/// they consume fake TCP through a raw stack on their TUN and therefore have no
/// ordinary TCP LISTEN socket for `ss -lnt` to find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhantunInstance {
    kind: PhantunInstanceKind,
    tun: PhantunTun,
}

impl PhantunInstance {
    fn program(&self) -> &'static str {
        match &self.kind {
            PhantunInstanceKind::Server { .. } => "phantun-server",
            PhantunInstanceKind::Client { .. } => "phantun-client",
        }
    }

    fn args(&self) -> Vec<String> {
        let (local, remote) = match &self.kind {
            PhantunInstanceKind::Server {
                tcp_port,
                forward_to_udp_port,
            } => (
                tcp_port.to_string(),
                format!("127.0.0.1:{forward_to_udp_port}"),
            ),
            PhantunInstanceKind::Client {
                listen_udp_port,
                remote_tcp_endpoint,
                ..
            } => (
                format!("127.0.0.1:{listen_udp_port}"),
                remote_tcp_endpoint.clone(),
            ),
        };
        vec![
            "--local".to_owned(),
            local,
            "--remote".to_owned(),
            remote,
            "--tun".to_owned(),
            self.tun.name.clone(),
            "--tun-local".to_owned(),
            self.tun.local.clone(),
            "--tun-peer".to_owned(),
            self.tun.peer.clone(),
        ]
    }

    fn label(&self) -> String {
        match &self.kind {
            PhantunInstanceKind::Server { tcp_port, .. } => {
                format!("服务端 tcp :{tcp_port}")
            }
            PhantunInstanceKind::Client {
                peer,
                listen_udp_port,
                ..
            } => format!("客户端 udp :{listen_udp_port} → {peer}"),
        }
    }
}

pub(crate) fn converge_linux_phantun(
    state_dir: &Path,
    desired: &DesiredArtifact,
    binaries: Option<&PhantunBinaries>,
) -> Result<(), String> {
    match desired {
        DesiredArtifact::Present { content, .. } => {
            let path = state_dir.join("phantun.json");
            write_private(&path, content)?;
            let _ = fs::remove_file(state_dir.join("phantun.disabled"));
            apply_phantun(state_dir, content, binaries)?;
            Ok(())
        }
        DesiredArtifact::Disabled { reason } => {
            stop_phantun();
            let _ = fs::remove_file("/tmp/brocade-agent-phantun.log");
            let _ = fs::remove_file(state_dir.join("phantun.json"));
            let _ = fs::remove_file(state_dir.join(PHANTUN_BOUNDED_LOG_MARKER));
            let _ = fs::remove_file(state_dir.join(PHANTUN_OLD_BOUNDED_LOG_MARKER));
            fs::write(state_dir.join("phantun.disabled"), reason)
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        DesiredArtifact::Unmanaged { .. } => Ok(()),
    }
}

/// Each phantun instance is a process taking command-line arguments only, with no
/// config file. So the approach here is stop-all then start-all — instance counts
/// are in the single digits, and diffing out which one changed saves nothing
/// while adding another piece of state that can drift.
///
/// Starting the processes is not enough. phantun sends and receives through a TUN
/// device and writes no firewall rules itself: on the client side outgoing
/// packets carry the TUN peer address (private) as source and never come back
/// without MASQUERADE; on the server side inbound TCP lands on the physical NIC
/// and nobody receives it without a DNAT to the TUN. That is what those two lines
/// in `phantun --help` about setting up SNAT/MASQUERADE rules mean.
///
/// The rules go in our own nft table (`inet brocade`), dropped and rebuilt
/// wholesale on each convergence. Otherwise the agent becomes something that
/// quietly edits the machine's global firewall, which is too large a blast
/// radius.
pub(crate) fn apply_phantun(
    state_dir: &Path,
    content: &str,
    binaries: Option<&PhantunBinaries>,
) -> Result<(), String> {
    let plan: serde_json::Value =
        serde_json::from_str(content).map_err(|error| error.to_string())?;
    prepare_phantun_binaries_for_plan(&plan, binaries)?;
    stop_phantun();

    let instances = phantun_instances_from_plan(&plan)?;
    prune_phantun_logs(state_dir, &instances)?;
    let mut rules = Vec::new();

    for instance in &instances {
        spawn_phantun(state_dir, instance)?;
        match &instance.kind {
            PhantunInstanceKind::Server { tcp_port, .. } => {
                // Inbound TCP lands on the physical NIC and must be steered to the TUN's
                // peer address for phantun to receive it. `iifname != "bt*"` cannot be
                // dropped: when this machine is also a client, its outgoing packets
                // target port 39743 too and pass through prerouting on their way into the
                // kernel from the client TUN, so without the qualifier this rule hijacks
                // them back to the local server TUN — the symptom is packets visible on
                // the TUN and none at all on the physical NIC. Mind conntrack when
                // changing this: established flows keep the old DNAT, and the phantun
                // client must restart for it to take effect.
                rules.push(format!(
                    "add rule inet brocade prerouting iifname != \"bt*\" tcp dport {tcp_port} dnat ip to {}",
                    instance.tun.peer
                ));
            }
            PhantunInstanceKind::Client { .. } => {
                // Outgoing packets carry the TUN peer (private) as source and never come
                // back unless it is rewritten to this machine's public address.
                rules.push(format!(
                    "add rule inet brocade postrouting ip saddr {} masquerade",
                    instance.tun.peer
                ));
            }
        }
    }

    if !rules.is_empty() {
        install_phantun_nat(&rules)?;
    }
    fs::write(state_dir.join(PHANTUN_BOUNDED_LOG_MARKER), b"dynamic\n")
        .map_err(|error| format!("failed to record bounded phantun logging: {error}"))?;
    let _ = fs::remove_file(state_dir.join(PHANTUN_OLD_BOUNDED_LOG_MARKER));
    // stop_phantun has closed every legacy descriptor, so this unlink releases the blocks now.
    let _ = fs::remove_file("/tmp/brocade-agent-phantun.log");
    Ok(())
}

/// Fetch everything the plan will need before convergence takes the backbone
/// lock.  Downloads are the slowest and least bounded part of installation; wg's
/// watchdog must not be locked out while bytes travel over the public network.
pub(crate) fn prepare_phantun_binaries(
    content: &str,
    binaries: Option<&PhantunBinaries>,
) -> Result<(), String> {
    let plan: serde_json::Value =
        serde_json::from_str(content).map_err(|error| error.to_string())?;
    prepare_phantun_binaries_for_plan(&plan, binaries)
}

fn prepare_phantun_binaries_for_plan(
    plan: &serde_json::Value,
    binaries: Option<&PhantunBinaries>,
) -> Result<(), String> {
    if !phantun_servers(plan).is_empty() {
        ensure_phantun_binary("phantun-server", binaries.map(|b| &b.server))?;
    }
    if plan
        .get("clients")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|clients| !clients.is_empty())
    {
        ensure_phantun_binary("phantun-client", binaries.map(|b| &b.client))?;
    }
    Ok(())
}

/// The `tun` field, split into (name, local, peer).
fn tun_fields(instance: &serde_json::Value) -> Result<(String, String, String), String> {
    let tun = instance.get("tun").ok_or("phantun.json: 实例缺 tun")?;
    let get = |key: &str| {
        tun.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("phantun.json: tun.{key} 缺失"))
    };
    Ok((get("name")?, get("local")?, get("peer")?))
}

/// If missing, fetch from the control plane and install after verifying sha256.
///
/// This path exists to rescue machines SSH cannot reach. The install script can
/// also install phantun, but that route needs someone able to log in — whereas
/// the whole point of the pull model is "the control plane can drive it even
/// though people cannot get in". Requiring SSH to install a mandatory binary
/// forfeits half of that.
///
/// The sha256 is not optional. Without it this is a path that downloads something
/// off the internet and runs it as root, so when the control plane has no
/// distribution source configured this errors out explicitly rather than guessing
/// an upstream address.
fn ensure_phantun_binary(program: &str, source: Option<&BinarySource>) -> Result<(), String> {
    if command_success("sh", &["-c", &format!("command -v {program}")])
        || Path::new(&format!("/usr/local/bin/{program}")).exists()
    {
        return Ok(());
    }
    let Some(source) = source else {
        return Err(format!(
            "{program} 不在这台机器上，控制面也没配 phantun 分发源\
             （BROCADE_PHANTUN_SERVER_URL / _CLIENT_URL 加对应的 SHA256）"
        ));
    };
    let url = shell_quote(&source.url);
    let sha = shell_quote(&source.sha256);
    let dest = format!("/usr/local/bin/{program}");
    run_shell_with_timeout(
        &format!(
            "set -eu\n\
         tmp=$(mktemp)\n\
         trap 'rm -f \"$tmp\"' EXIT\n\
         curl -fsSL --connect-timeout 15 --max-time 120 {url} -o \"$tmp\"\n\
         got=$(sha256sum \"$tmp\" | cut -d' ' -f1)\n\
         if [ \"$got\" != {sha} ]; then\n\
           echo \"{program} sha256 不匹配\" >&2\n\
           exit 1\n\
         fi\n\
         install -m 0755 \"$tmp\" {dest}"
        ),
        Duration::from_secs(130),
    )?;
    println!("phantun: 已取得 {program}");
    Ok(())
}

fn spawn_phantun(state_dir: &Path, instance: &PhantunInstance) -> Result<(), String> {
    let program = instance.program();
    let args = instance.args();
    let quoted = args
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let log_path = phantun_log_path(state_dir, instance);
    let sink = crate::logcap::command(&log_path, state_dir)?;
    let pipeline = shell_quote(&format!("{program} {quoted} 2>&1 | {sink}"));
    run_shell(&format!(
        "set -eu\n\
         nohup sh -c {pipeline} >/dev/null 2>&1 &\n\
         sleep 0.4"
    ))?;
    Ok(())
}

fn phantun_log_path(state_dir: &Path, instance: &PhantunInstance) -> std::path::PathBuf {
    let tun = instance
        .tun
        .name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    state_dir
        .join("logs")
        .join(format!("{}-{tun}.log", instance.program()))
}

fn prune_phantun_logs(state_dir: &Path, instances: &[PhantunInstance]) -> Result<(), String> {
    let directory = state_dir.join("logs");
    let mut wanted = BTreeSet::new();
    for instance in instances {
        let path = phantun_log_path(state_dir, instance);
        if !wanted.insert(path.clone()) {
            return Err(format!(
                "phantun plan repeats TUN/log identity {}",
                instance.tun.name
            ));
        }
        let mut archive = path.as_os_str().to_os_string();
        archive.push(".1");
        wanted.insert(std::path::PathBuf::from(archive));
    }
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read {}: {error}", directory.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {}: {error}", directory.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ours = (name.starts_with("phantun-server-") || name.starts_with("phantun-client-"))
            && (name.ends_with(".log") || name.ends_with(".log.1"));
        if ours && !wanted.contains(&path) {
            fs::remove_file(&path)
                .map_err(|error| format!("remove {}: {error}", path.display()))?;
        }
    }
    Ok(())
}

fn install_phantun_nat(rules: &[String]) -> Result<(), String> {
    // The rules must go to `nft -f -` together with the table definition. Outside
    // the heredoc the shell would try to execute `add rule ...` as a command and
    // report `add: not found`.
    let body = rules.join("\n");
    run_shell(&format!(
        "set -eu\n\
         command -v nft >/dev/null 2>&1 || {{ echo '需要 nft（nftables）来给 phantun 配 NAT' >&2; exit 1; }}\n\
         sysctl -qw net.ipv4.ip_forward=1\n\
         nft delete table inet brocade 2>/dev/null || true\n\
         nft -f - <<'NFT'\n\
table inet brocade {{\n\
  chain prerouting {{ type nat hook prerouting priority -100; }}\n\
  chain postrouting {{ type nat hook postrouting priority 100; }}\n\
  chain forward {{ type filter hook forward priority 0; }}\n\
}}\n\
add rule inet brocade forward iifname \"{TUN_PREFIX}*\" accept\n\
add rule inet brocade forward oifname \"{TUN_PREFIX}*\" accept\n\
{body}\n\
NFT\n"
    ))?;
    allow_forward_through_foreign_chains()?;
    Ok(())
}

/// Prefix for phantun's TUN device names. Used as nft's wildcard prefix so that
/// one rule covers every instance.
const TUN_PREFIX: &str = "bt";

/// When somebody else's forward chain is `policy drop`, the accept in our own
/// table cannot save us.
///
/// nftables runs every base chain on a hook and any single drop is final; our
/// accept only governs our own chain. Docker sets the forward policy to drop on
/// install — so on a machine with Docker every phantun packet leaving the TUN is
/// dropped, and the symptom is a client stuck on `Unable to connect to remote`
/// while the NAT rules, the TUN device, and the processes all look fine.
///
/// The only way in is the extension point it leaves, `DOCKER-USER`. Insert only
/// rules carrying our marker and delete only those — find handles by comment and
/// delete them, never flush the chain, which belongs to someone else.
///
/// Without Docker this does nothing: the two accepts in our own table suffice.
fn allow_forward_through_foreign_chains() -> Result<(), String> {
    let _ = run_shell(&format!(
        "nft list chain ip filter DOCKER-USER >/dev/null 2>&1 || exit 0\n\
         nft -a list chain ip filter DOCKER-USER 2>/dev/null \
           | grep 'comment \"brocade\"' \
           | grep -o 'handle [0-9]*' \
           | awk '{{print $2}}' \
           | while read -r h; do nft delete rule ip filter DOCKER-USER handle $h || true; done\n\
         nft insert rule ip filter DOCKER-USER iifname \"{TUN_PREFIX}*\" accept comment \"brocade\"\n\
         nft insert rule ip filter DOCKER-USER oifname \"{TUN_PREFIX}*\" accept comment \"brocade\"\n\
         echo 'phantun: 已在 DOCKER-USER 里放行 {TUN_PREFIX}* 的转发' >&2"
    ))?;
    Ok(())
}

// Kill only the two program names we started, delete only our own nft table.
// phantun's TUN devices are left for it to clean up — once the process is gone
// the device is useless anyway, and forcing the deletion risks removing someone
// else's.
fn stop_phantun() {
    let _ = run_shell("nft delete table inet brocade 2>/dev/null || true");
    // The two rules inserted into someone else's chain must be cleaned up too:
    // delete by marker, touch nothing else
    let _ = run_shell(
        "nft list chain ip filter DOCKER-USER >/dev/null 2>&1 || exit 0\n\
         nft -a list chain ip filter DOCKER-USER 2>/dev/null \
           | grep 'comment \"brocade\"' | grep -o 'handle [0-9]*' | awk '{print $2}' \
           | while read -r h; do nft delete rule ip filter DOCKER-USER handle \"$h\" || true; done",
    );
    let _ = run_shell(
        "pkill -x phantun-server 2>/dev/null || true\n\
         pkill -x phantun-client 2>/dev/null || true\n\
         for _ in $(seq 1 20); do\n\
           pgrep -x phantun-server >/dev/null 2>&1 || pgrep -x phantun-client >/dev/null 2>&1 || break\n\
           sleep 0.25\n\
         done",
    );
}

pub(crate) fn observe_linux_phantun(
    state_dir: &Path,
    desired: &DesiredArtifact,
) -> AppliedArtifactState {
    if matches!(desired, DesiredArtifact::Unmanaged { .. }) {
        return AppliedArtifactState::Unmanaged;
    }
    let path = state_dir.join("phantun.json");
    let disabled_path = state_dir.join("phantun.disabled");
    match (path.exists(), disabled_path.exists()) {
        (true, true) => artifact_dirty("phantun.json 和 phantun.disabled 同时存在"),
        (true, false) => {
            // A written file does not mean a running process. Instances in the
            // plan with no process at all is dirty, not present — exactly the
            // "console says converged, nothing running on the machine" case
            // already hit with xray.
            if phantun_wanted(&path) && !phantun_running() {
                return artifact_dirty("phantun.json 里有实例，但一个 phantun 进程都没跑");
            }
            if !state_dir.join(PHANTUN_BOUNDED_LOG_MARKER).exists() {
                return artifact_dirty("phantun 仍在使用旧的无限日志");
            }
            present_file_state(&path, "phantun.json")
        }
        (false, true) => {
            if phantun_running() {
                artifact_dirty("phantun 已停用，但进程还在跑")
            } else {
                AppliedArtifactState::Disabled
            }
        }
        (false, false) => AppliedArtifactState::Unknown,
    }
}

/// The servers to stand up according to `phantun.json`.
///
/// The current format is a `servers` array — one public machine may host a server
/// for each of several peers behind NAT (`ir::system::LinkWrap`). The old format
/// was a singular `server` object, accepted here as well: what sits in state_dir
/// may predate the upgrade, and failing to read it amounts to "this machine needs
/// no phantun", which is the hardest kind of silent failure to find.
pub(crate) fn phantun_servers(plan: &serde_json::Value) -> Vec<&serde_json::Value> {
    if let Some(servers) = plan.get("servers").and_then(serde_json::Value::as_array) {
        return servers.iter().collect();
    }
    plan.get("server").into_iter().collect()
}

fn required_u16(instance: &serde_json::Value, key: &str, location: &str) -> Result<u16, String> {
    let value = instance
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("phantun.json: {location}.{key} 缺失"))?;
    u16::try_from(value).map_err(|_| format!("phantun.json: {location}.{key} 超出端口范围"))
}

fn required_string(
    instance: &serde_json::Value,
    key: &str,
    location: &str,
) -> Result<String, String> {
    instance
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("phantun.json: {location}.{key} 缺失"))
}

fn phantun_instances_from_plan(plan: &serde_json::Value) -> Result<Vec<PhantunInstance>, String> {
    let mut out = Vec::new();
    for server in phantun_servers(plan) {
        let tcp_port = required_u16(server, "tcp_port", "servers[]")?;
        let forward_to_udp_port = required_u16(server, "forward_to_udp_port", "servers[]")?;
        let (name, local, peer) = tun_fields(server)?;
        out.push(PhantunInstance {
            kind: PhantunInstanceKind::Server {
                tcp_port,
                forward_to_udp_port,
            },
            tun: PhantunTun { name, local, peer },
        });
    }
    for client in plan
        .get("clients")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let listen_udp_port = required_u16(client, "listen_udp_port", "clients[]")?;
        let remote_tcp_endpoint = required_string(client, "remote_tcp_endpoint", "clients[]")?;
        let (name, local, tun_peer) = tun_fields(client)?;
        out.push(PhantunInstance {
            kind: PhantunInstanceKind::Client {
                peer: client
                    .get("peer")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
                    .to_owned(),
                listen_udp_port,
                remote_tcp_endpoint,
            },
            tun: PhantunTun {
                name,
                local,
                peer: tun_peer,
            },
        });
    }
    Ok(out)
}

pub(crate) fn phantun_instances(path: &Path) -> Result<Vec<PhantunInstance>, String> {
    let content = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let plan: serde_json::Value =
        serde_json::from_str(&content).map_err(|error| error.to_string())?;
    phantun_instances_from_plan(&plan)
}

fn command_line_matches(instance: &PhantunInstance, argv: &[String]) -> bool {
    let Some(program) = argv
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    program == instance.program() && argv[1..] == instance.args()
}

/// Match every planned process by its full argument vector. A global `pgrep` is
/// insufficient: with two servers, or a server and a client, one survivor would
/// hide the instance that died.
fn phantun_instance_running(instance: &PhantunInstance) -> Option<bool> {
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let pid = entry.file_name();
        if !pid
            .to_str()
            .is_some_and(|pid| pid.bytes().all(|byte| byte.is_ascii_digit()))
        {
            continue;
        }
        let Ok(content) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv = content
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect::<Vec<_>>();
        if command_line_matches(instance, &argv) {
            return Some(true);
        }
    }
    Some(false)
}

fn phantun_tun_present(instance: &PhantunInstance) -> Option<bool> {
    Path::new("/sys/class/net")
        .join(&instance.tun.name)
        .try_exists()
        .ok()
}

/// The client owns an ordinary UDP listener. The server is intentionally absent:
/// its fake TCP stack consumes packets from the TUN and never creates a TCP LISTEN
/// socket.
pub(crate) fn phantun_runtime_probes(
    instance: &PhantunInstance,
) -> Vec<(String, String, Option<bool>)> {
    let label = instance.label();
    let mut probes = vec![
        (
            format!("{label} 的进程没按计划运行"),
            format!("{label} 的进程状态无法检查"),
            phantun_instance_running(instance),
        ),
        (
            format!("{label} 的 TUN {} 不存在", instance.tun.name),
            format!("{label} 的 TUN {} 无法检查", instance.tun.name),
            phantun_tun_present(instance),
        ),
    ];
    if let PhantunInstanceKind::Client {
        listen_udp_port, ..
    } = &instance.kind
    {
        probes.push((
            format!("{label} 没在监听"),
            format!("{label} 的监听状态无法检查"),
            udp_bound(*listen_udp_port),
        ));
    }
    probes
}

/// `None` means it could not be determined (neither `ss` nor `netstat` present),
/// not "nobody is listening".
pub(crate) fn udp_bound(port: u16) -> Option<bool> {
    let out = run_shell(&format!(
        "if command -v ss >/dev/null 2>&1; then \
           ss -lnu 2>/dev/null | grep -q ':{port} ' && echo yes || echo no; \
         elif command -v netstat >/dev/null 2>&1; then \
           netstat -lnu 2>/dev/null | grep -q ':{port} ' && echo yes || echo no; \
         else \
           echo unknown; \
         fi"
    ))
    .ok()?;
    match out.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

pub(crate) fn phantun_wanted(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(plan) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    !phantun_servers(&plan).is_empty()
        || plan
            .get("clients")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|clients| !clients.is_empty())
}

pub(crate) fn phantun_running() -> bool {
    command_success("pgrep", &["-x", "phantun-server"])
        || command_success("pgrep", &["-x", "phantun-client"])
}

/// Whether the kernel state installed beside the processes is still standing.
///
/// A convergence lands three things, not one: the processes, the `inet brocade` table with
/// `ip_forward` alongside it, and two accepts planted in Docker's `DOCKER-USER` chain. Only the
/// first dies in a way `phantun_running` can see, and the other two go without it noticing —
/// `nft flush ruleset`, a firewalld or nftables.service reload, another tool writing `ip_forward`
/// back to 0, or, most often, a Docker daemon restart, which rebuilds its own chains and takes the
/// planted accepts with them. The processes stay up through every one of those, traffic stops, and
/// an idle round that asks only "is it running" answers yes.
///
/// Only worth asking where phantun is actually wanted: with no instances `apply_phantun` writes no
/// rules at all, and a missing table is then the correct state rather than drift.
pub(crate) fn phantun_nat_intact() -> bool {
    command_success("nft", &["list", "table", "inet", "brocade"])
        && ip_forward_enabled()
        && docker_forward_still_allowed()
}

/// Read out of procfs rather than shelling out to `sysctl`: one file, and no dependency on a binary
/// that minimal images leave out. Absent means no IPv4 forwarding in this kernel, which counts as
/// not enabled rather than as an error to propagate.
fn ip_forward_enabled() -> bool {
    fs::read_to_string("/proc/sys/net/ipv4/ip_forward").is_ok_and(|value| value.trim() == "1")
}

/// The chain not existing is not drift — it means Docker is not on this machine, and
/// `allow_forward_through_foreign_chains` planted nothing to lose. Drift is the chain being there
/// with our marker gone.
fn docker_forward_still_allowed() -> bool {
    command_success(
        "sh",
        &[
            "-c",
            "nft list chain ip filter DOCKER-USER >/dev/null 2>&1 || exit 0\n\
             nft list chain ip filter DOCKER-USER 2>/dev/null | grep -q 'comment \"brocade\"'",
        ],
    )
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf};

    use brocade_deployment::plan::{AppliedArtifactState, DesiredArtifact};
    use serde_json::json;

    use super::{
        command_line_matches, converge_linux_phantun, observe_linux_phantun, phantun_instances,
        phantun_log_path, phantun_runtime_probes, phantun_servers, phantun_wanted,
        prune_phantun_logs, tun_fields, PhantunInstance, PhantunInstanceKind, PhantunTun,
    };

    fn state_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "brocade-agent-phantun-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The old format is a singular `server` object, the new one a `servers`
    /// array. What landed in state_dir before the upgrade is the old shape, and
    /// failing to read it amounts to "this machine needs no phantun" — no server
    /// started, no DNAT written, and the console showing everything as fine. That
    /// is the hardest kind of silent failure to find, so both spellings count.
    #[test]
    fn both_the_old_singular_server_and_the_new_array_are_read() {
        let new = json!({ "servers": [{ "tcp_port": 39743 }, { "tcp_port": 39744 }] });
        assert_eq!(phantun_servers(&new).len(), 2);

        let old = json!({ "server": { "tcp_port": 39743 } });
        let found = phantun_servers(&old);
        assert_eq!(found.len(), 1, "老格式的单数 server 也要认");
        assert_eq!(found[0]["tcp_port"], 39743);

        // Only with neither key present does this machine truly host no
        // server.
        assert!(phantun_servers(&json!({ "clients": [] })).is_empty());
    }

    /// With `servers` present, `server` is not consulted. Both at once can only
    /// mean corruption, and treating the old key as a supplement conjures an extra
    /// process and an extra DNAT out of nothing.
    #[test]
    fn the_new_array_wins_when_both_spellings_are_present() {
        let both = json!({
            "servers": [{ "tcp_port": 39743 }],
            "server": { "tcp_port": 1 },
        });
        let found = phantun_servers(&both);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["tcp_port"], 39743);
    }

    /// An empty array means "this machine needs no phantun", not "unreadable".
    /// observe uses it to decide whether to report dirty, and getting it backwards
    /// declares every round unconverged.
    #[test]
    fn wanted_is_false_for_an_empty_plan_and_true_for_either_side() {
        let dir = state_dir("wanted");

        let path = dir.join("phantun.json");
        fs::write(&path, json!({ "servers": [], "clients": [] }).to_string()).unwrap();
        assert!(!phantun_wanted(&path));

        fs::write(
            &path,
            json!({ "servers": [{ "tcp_port": 39743 }] }).to_string(),
        )
        .unwrap();
        assert!(phantun_wanted(&path), "只有服务端也算要跑");

        fs::write(
            &path,
            json!({ "clients": [{ "listen_udp_port": 51820 }] }).to_string(),
        )
        .unwrap();
        assert!(phantun_wanted(&path), "只有客户端也算要跑");

        let _ = fs::remove_dir_all(dir);
    }

    /// A missing file, or content that is not JSON, both count as "not needed".
    /// Returning true here would pin the node at dirty forever over one unreadable
    /// file.
    #[test]
    fn wanted_is_false_when_the_plan_is_missing_or_unparsable() {
        let dir = state_dir("wanted-broken");

        assert!(!phantun_wanted(&dir.join("nothing.json")));

        let path = dir.join("phantun.json");
        fs::write(&path, "{ 这不是 JSON").unwrap();
        assert!(!phantun_wanted(&path));

        let _ = fs::remove_dir_all(dir);
    }

    /// The process plan carries enough information to distinguish two instances of
    /// the same binary and to verify the exact command that convergence launched.
    #[test]
    fn instances_keep_server_and_client_commands_distinct() {
        let dir = state_dir("instances");
        let path = dir.join("phantun.json");
        fs::write(
            &path,
            json!({
                "servers": [{
                    "tcp_port": 39743,
                    "forward_to_udp_port": 51820,
                    "tun": { "name": "bts0", "local": "192.168.200.1", "peer": "192.168.200.2" }
                }],
                "clients": [{
                    "listen_udp_port": 29000,
                    "remote_tcp_endpoint": "198.51.100.8:39743",
                    "peer": "hk-01",
                    "tun": { "name": "btc1", "local": "192.168.200.5", "peer": "192.168.200.6" }
                }],
            })
            .to_string(),
        )
        .unwrap();

        let found = phantun_instances(&path).unwrap();
        assert_eq!(found.len(), 2);
        assert!(matches!(
            found[0].kind,
            PhantunInstanceKind::Server {
                tcp_port: 39743,
                forward_to_udp_port: 51820
            }
        ));
        assert_eq!(found[0].label(), "服务端 tcp :39743");
        assert_eq!(
            found[0].args()[..4],
            ["--local", "39743", "--remote", "127.0.0.1:51820"]
        );
        assert!(matches!(
            found[1].kind,
            PhantunInstanceKind::Client {
                listen_udp_port: 29000,
                ..
            }
        ));
        assert_eq!(found[1].label(), "客户端 udp :29000 → hk-01");

        let _ = fs::remove_dir_all(dir);
    }

    /// A partial instance is not silently skipped. Otherwise its process can be
    /// absent while health declares every instance it happened to understand healthy.
    #[test]
    fn instances_reject_an_entry_without_its_port() {
        let dir = state_dir("instances-partial");
        let path = dir.join("phantun.json");
        fs::write(
            &path,
            json!({
                "servers": [{ "no_port": true }],
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(
            phantun_instances(&path).unwrap_err(),
            "phantun.json: servers[].tcp_port 缺失"
        );

        let _ = fs::remove_dir_all(dir);
    }

    /// A raw-TCP server has process and TUN probes, but deliberately no ordinary
    /// TCP socket probe. The client adds a third probe for its real UDP listener.
    #[test]
    fn only_clients_are_probed_for_a_listen_socket() {
        let dir = state_dir("runtime-probes");
        let path = dir.join("phantun.json");
        fs::write(
            &path,
            json!({
                "servers": [{
                    "tcp_port": 39743,
                    "forward_to_udp_port": 51820,
                    "tun": { "name": "bts0", "local": "192.168.200.1", "peer": "192.168.200.2" }
                }],
                "clients": [{
                    "peer": "hk-01",
                    "listen_udp_port": 29000,
                    "remote_tcp_endpoint": "198.51.100.8:39743",
                    "tun": { "name": "btc1", "local": "192.168.200.5", "peer": "192.168.200.6" }
                }]
            })
            .to_string(),
        )
        .unwrap();

        let instances = phantun_instances(&path).unwrap();
        assert_eq!(phantun_runtime_probes(&instances[0]).len(), 2);
        assert_eq!(phantun_runtime_probes(&instances[1]).len(), 3);

        let mut command = vec!["/usr/local/bin/phantun-server".to_owned()];
        command.extend(instances[0].args());
        assert!(command_line_matches(&instances[0], &command));
        command[2] = "39744".to_owned();
        assert!(!command_line_matches(&instances[0], &command));

        let _ = fs::remove_dir_all(dir);
    }

    /// Any of the three tun fields missing is an error. Silently filling in a
    /// default name would have two instances fight over the same TUN device, and
    /// the symptom is the later one failing to start with an error that never
    /// mentions the name collision.
    #[test]
    fn tun_fields_name_the_missing_key_instead_of_guessing() {
        let ok =
            json!({ "tun": { "name": "bt0", "local": "192.168.201.1", "peer": "192.168.201.2" } });
        assert_eq!(
            tun_fields(&ok).unwrap(),
            (
                "bt0".to_owned(),
                "192.168.201.1".to_owned(),
                "192.168.201.2".to_owned()
            )
        );

        let missing_peer = json!({ "tun": { "name": "bt0", "local": "192.168.201.1" } });
        assert_eq!(
            tun_fields(&missing_peer).unwrap_err(),
            "phantun.json: tun.peer 缺失"
        );

        assert_eq!(
            tun_fields(&json!({})).unwrap_err(),
            "phantun.json: 实例缺 tun"
        );
    }

    /// Both marker files present means the previous convergence died midway. What
    /// the machine is actually running cannot be stated, so the only answer is
    /// dirty and let the control plane push again — believing either one gives
    /// "disabled" and "running" even odds of being right.
    #[test]
    fn both_marker_files_present_is_dirty() {
        let dir = state_dir("observe-both");
        fs::write(dir.join("phantun.json"), "{}").unwrap();
        fs::write(dir.join("phantun.disabled"), "停用").unwrap();

        let state = observe_linux_phantun(
            &dir,
            &DesiredArtifact::Present {
                content: "{}".to_owned(),
                sha256: "ignored".to_owned(),
            },
        );
        assert!(
            matches!(state, AppliedArtifactState::Dirty { .. }),
            "两个标记同时存在必须是 dirty，实际 {state:?}"
        );

        let _ = fs::remove_dir_all(dir);
    }

    /// No marker at all is Unknown, not Disabled. The machine may have just been
    /// installed and never converged, and reporting Disabled declares on its
    /// behalf that it was shut off as planned.
    #[test]
    fn no_marker_file_at_all_is_unknown() {
        let dir = state_dir("observe-none");
        let state = observe_linux_phantun(
            &dir,
            &DesiredArtifact::Present {
                content: "{}".to_owned(),
                sha256: "ignored".to_owned(),
            },
        );
        assert_eq!(state, AppliedArtifactState::Unknown);
        let _ = fs::remove_dir_all(dir);
    }

    /// When it is not ours to manage, the disk must not even be glanced at:
    /// state_dir may hold a phantun.json left by a previous operator, and
    /// reporting state from it claims someone else's work as our own.
    #[test]
    fn unmanaged_short_circuits_before_touching_the_disk() {
        let dir = state_dir("observe-unmanaged");
        fs::write(dir.join("phantun.json"), "{}").unwrap();
        fs::write(dir.join("phantun.disabled"), "停用").unwrap();

        let state = observe_linux_phantun(
            &dir,
            &DesiredArtifact::Unmanaged {
                reason: "不归这台管".to_owned(),
            },
        );
        assert_eq!(state, AppliedArtifactState::Unmanaged);

        let _ = fs::remove_dir_all(dir);
    }

    // There is deliberately no test for the Disabled branch: it necessarily calls
    // stop_phantun(), which really does run `nft delete table inet brocade` and
    // pkill. As non-root the failures are swallowed; running the tests as root
    // would delete this machine's actual table. Testing it requires separating
    // "remove the files" from "stop the processes" first, which is a structural
    // change, not a new test.

    /// Unmanaged does nothing. Writing the file would have the next observe report
    /// it as our own state.
    #[test]
    fn unmanaged_converge_writes_nothing() {
        let dir = state_dir("converge-unmanaged");

        converge_linux_phantun(
            &dir,
            &DesiredArtifact::Unmanaged {
                reason: "不归这台管".to_owned(),
            },
            None,
        )
        .unwrap();

        assert!(!dir.join("phantun.json").exists());
        assert!(!dir.join("phantun.disabled").exists());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn retired_instance_logs_are_removed_without_touching_unrelated_files() {
        let dir = state_dir("prune-logs");
        let logs = dir.join("logs");
        fs::create_dir_all(&logs).unwrap();
        let instance = PhantunInstance {
            kind: PhantunInstanceKind::Client {
                peer: "peer".to_owned(),
                listen_udp_port: 3000,
                remote_tcp_endpoint: "192.0.2.1:443".to_owned(),
            },
            tun: PhantunTun {
                name: "bt/0".to_owned(),
                local: "10.0.0.1".to_owned(),
                peer: "10.0.0.2".to_owned(),
            },
        };
        let keep = phantun_log_path(&dir, &instance);
        fs::write(&keep, "current").unwrap();
        fs::write(logs.join("phantun-client-retired.log"), "old").unwrap();
        fs::write(logs.join("notes.log"), "operator").unwrap();

        prune_phantun_logs(&dir, &[instance]).unwrap();
        assert!(keep.exists());
        assert!(!logs.join("phantun-client-retired.log").exists());
        assert!(logs.join("notes.log").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
