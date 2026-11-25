//! Bounded, credential-redacted entry execution and child-process logs.
//! Readers always drain the child pipe; disk failures never stop the data plane.
use crate::fsutil::{atomic_write_private, read_bounded};
use brocade_deployment::cvm::*;
use std::path::{Path, PathBuf};
use std::{
    collections::BTreeMap,
    io::Read,
    os::unix::net::UnixStream,
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
};

const CAPACITY: usize = 256;
const HANDSHAKE_TIMEOUT: u64 = 120;
#[derive(Default)]
struct Runtime {
    data: NetworkDiagnostics,
    generation: Option<u64>,
    error: Option<String>,
    disk_failed: bool,
    applied: Option<(u64, usize, usize)>,
    waiting: BTreeMap<String, std::time::Instant>,
}
static SAVE: Mutex<()> = Mutex::new(());
static LOGS: OnceLock<Mutex<BTreeMap<PathBuf, Runtime>>> = OnceLock::new();
fn registry() -> &'static Mutex<BTreeMap<PathBuf, Runtime>> {
    LOGS.get_or_init(Mutex::default)
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn path(root: &Path) -> PathBuf {
    root.join("cvm-network-diagnostics.json")
}
fn runtime<'a>(all: &'a mut BTreeMap<PathBuf, Runtime>, root: &Path) -> &'a mut Runtime {
    all.entry(root.to_owned()).or_insert_with(|| Runtime {
        data: read_bounded(&path(root), 2 * 1024 * 1024)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<NetworkDiagnostics>(&bytes).ok())
            .filter(|d| d.validate().is_ok())
            .unwrap_or_default(),
        ..Default::default()
    })
}
pub(in crate::cvm) fn clip(value: &str, limit: usize) -> String {
    let mut out = String::new();
    for c in value.chars().filter(|c| !c.is_control() || *c == '\t') {
        if out.len() + c.len_utf8() > limit {
            break;
        }
        out.push(c);
    }
    out
}
pub(in crate::cvm) fn secrets(values: &[String]) -> Vec<String> {
    fn collect(value: &serde_json::Value, output: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) => output.push(s.clone()),
            serde_json::Value::Object(map) => {
                for value in map.values() {
                    collect(value, output);
                }
            }
            serde_json::Value::Array(items) => {
                for value in items {
                    collect(value, output);
                }
            }
            _ => {}
        }
    }
    let mut out = values.to_vec();
    for value in values {
        if let Ok(parsed) = serde_json::from_str(value) {
            collect(&parsed, &mut out);
        }
    }
    let lines = out
        .iter()
        .flat_map(|s| s.lines())
        .filter(|s| s.len() >= 16)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    out.extend(lines);
    out.retain(|s| s.len() >= 8);
    out.sort_by_key(|s| std::cmp::Reverse(s.len()));
    out.dedup();
    out
}
pub(in crate::cvm) fn redact(value: &str, secrets: &[String]) -> String {
    let mut value = value.to_owned();
    for secret in secrets {
        value = value.replace(secret, "[已隐藏]");
    }
    clip(&value, 1000)
}
/// wg may echo a rejected private key; report a shared safe reason, never its raw output.
pub(in crate::cvm) fn wireguard_error(stage: &'static str, error: &str) -> String {
    let message = error.to_ascii_lowercase();
    let reason = if message.contains("permission denied") || message.contains("(os error 13)") {
        "配置访问权限被拒绝，请检查文件权限及 AppArmor 等系统安全策略"
    } else if message.contains("operation not permitted") || message.contains("(os error 1)") {
        "系统拒绝网络配置权限，请检查网络管理权限及系统安全策略"
    } else if message.contains("timed out") || message.contains("(os error 110)") {
        "配置命令超时，请检查本机执行状态"
    } else if message.contains("address already in use") {
        "监听地址或端口已被占用"
    } else if message.contains("no such device")
        || (message.contains("cannot open network namespace")
            && message.contains("no such file or directory"))
    {
        "WireGuard 网卡或网络命名空间不存在"
    } else if message.contains("no such file or directory") || message.contains("command not found")
    {
        "程序或配置文件不存在，请检查 WireGuard 安装及配置路径"
    } else if message.contains("key is not the correct length or format")
        || message.contains("invalid key")
    {
        "密钥格式无效，请检查 WireGuard 凭据"
    } else {
        "配置命令失败，请检查 Agent 与系统日志"
    };
    format!("{stage}：{reason}")
}
pub(super) fn command_error(
    root: &Path,
    id: Option<&str>,
    action: &str,
    error: &str,
    redactions: &[String],
) -> String {
    let message = clip(&format!("{action}：{}", redact(error, redactions)), 480);
    record(
        root,
        id,
        NetworkLogSource::Agent,
        NetworkLogLevel::Error,
        &message,
    );
    message
}
fn push(data: &mut NetworkDiagnostics, event: NetworkLogEvent) {
    if data.events.len() == CAPACITY {
        data.events.remove(0);
        data.dropped_events = data.dropped_events.saturating_add(1);
    }
    data.events.push(event);
}
pub(in crate::cvm) fn record(
    root: &Path,
    tunnel: Option<&str>,
    source: NetworkLogSource,
    level: NetworkLogLevel,
    message: &str,
) {
    if let Ok(mut all) = registry().lock() {
        push(
            &mut runtime(&mut all, root).data,
            NetworkLogEvent {
                at: now(),
                resource_id: tunnel.map(str::to_owned),
                source,
                level,
                message: clip(message, 1000),
            },
        );
    }
}
pub(super) fn begin(root: &Path, desired: &EntryDesired) {
    if let Ok(mut all) = registry().lock() {
        let state = runtime(&mut all, root);
        if state.generation != Some(desired.generation) {
            state.generation = Some(desired.generation);
            push(
                &mut state.data,
                NetworkLogEvent {
                    at: now(),
                    resource_id: None,
                    source: NetworkLogSource::Agent,
                    level: NetworkLogLevel::Info,
                    message: format!(
                        "收到入口配置：版本 {}，{} 条转发、{} 条投递、{} 条通道",
                        desired.generation,
                        desired.rules.len(),
                        desired.deliveries.len(),
                        desired.tunnels.len()
                    ),
                },
            );
        }
    }
}
/// Adds progress and a finite handshake-wait verdict without changing retry behavior.
/// Only logging is best-effort; a tunnel which is not authenticated never becomes ready here.
pub(super) fn finish(root: &Path, desired: &EntryDesired, report: &mut EntryNodeReport) {
    let Ok(mut all) = registry().lock() else {
        return;
    };
    let state = runtime(&mut all, root);
    let at = now();
    if state.error != report.error {
        state.error.clone_from(&report.error);
        if let Some(error) = &report.error {
            push(
                &mut state.data,
                NetworkLogEvent {
                    at,
                    resource_id: None,
                    source: NetworkLogSource::Agent,
                    level: NetworkLogLevel::Error,
                    message: clip(error, 1000),
                },
            );
        } else {
            push(
                &mut state.data,
                NetworkLogEvent {
                    at,
                    resource_id: None,
                    source: NetworkLogSource::Agent,
                    level: NetworkLogLevel::Info,
                    message: "本机入口规则已确认应用".into(),
                },
            );
        }
    }
    let rules = report.rules.iter().filter(|r| r.configured).count();
    let deliveries = report.deliveries.iter().filter(|r| r.configured).count();
    let applied = (desired.generation, rules, deliveries);
    if report.error.is_none() && state.applied != Some(applied) {
        state.applied = Some(applied);
        let complete = rules == desired.rules.len() && deliveries == desired.deliveries.len();
        push(
            &mut state.data,
            NetworkLogEvent {
                at,
                resource_id: None,
                source: NetworkLogSource::Agent,
                level: NetworkLogLevel::Info,
                message: format!(
                    "入口配置 v{}：转发 {}/{}，宿主投递 {}/{}；{}",
                    desired.generation,
                    rules,
                    desired.rules.len(),
                    deliveries,
                    desired.deliveries.len(),
                    if complete {
                        "规则应用已确认"
                    } else {
                        "等待其余通道就绪"
                    }
                ),
            },
        );
    }
    let mut progress = Vec::new();
    for tunnel in &desired.tunnels {
        let previous = state.data.resources.iter().find(|p| p.id == tunnel.id);
        let mut observation = report.tunnels.iter_mut().find(|t| t.id == tunnel.id);
        let connected = observation.as_ref().is_some_and(|t| t.connected);
        let since = previous
            .filter(|p| (p.phase == NetworkResourcePhase::Connected) == connected)
            .map_or(at, |p| p.since);
        let waiting = if connected {
            state.waiting.remove(&tunnel.id);
            0
        } else {
            state
                .waiting
                .entry(tunnel.id.clone())
                .or_insert_with(std::time::Instant::now)
                .elapsed()
                .as_secs()
        };
        let name = if tunnel.offer.kind == TransportKind::Xray {
            "Xray 认证连接"
        } else if tunnel.wireguard_transport.is_udp() {
            "WireGuard 握手"
        } else {
            "Phantun / WireGuard 握手"
        };
        if !connected && waiting >= HANDSHAKE_TIMEOUT && report.error.is_none() {
            if let Some(t) = observation.as_deref_mut() {
                if t.error.is_none() {
                    t.error = Some(format!(
                        "{name}等待超过 120 秒；请查看两端日志。Agent 将继续重试"
                    ));
                }
            }
        }
        let error = report
            .error
            .as_ref()
            .or_else(|| observation.as_ref().and_then(|t| t.error.as_ref()));
        let phase = if error.is_some() {
            NetworkResourcePhase::Failed
        } else if connected {
            NetworkResourcePhase::Connected
        } else if observation.is_some() {
            NetworkResourcePhase::Waiting
        } else {
            NetworkResourcePhase::Preparing
        };
        let message = error.cloned().unwrap_or_else(|| {
            if connected {
                format!("{name}已建立")
            } else {
                format!("等待{name}；请检查对端进程、承载端口及防火墙")
            }
        });
        if previous.is_none_or(|p| p.phase != phase || p.message != message) {
            push(
                &mut state.data,
                NetworkLogEvent {
                    at,
                    resource_id: Some(tunnel.id.clone()),
                    source: NetworkLogSource::Agent,
                    level: if phase == NetworkResourcePhase::Failed {
                        NetworkLogLevel::Error
                    } else {
                        NetworkLogLevel::Info
                    },
                    message: clip(&message, 1000),
                },
            );
        }
        progress.push(NetworkResourceProgress {
            id: tunnel.id.clone(),
            phase,
            since,
            waiting_seconds: waiting,
            message: clip(&message, 480),
        });
    }
    state.waiting.retain(|id, _| {
        !id.starts_with("entry-tunnel-") || desired.tunnels.iter().any(|t| &t.id == id)
    });
    state
        .data
        .resources
        .retain(|p| !p.id.starts_with("entry-tunnel-"));
    state.data.resources.extend(progress);
    let excess = state.data.resources.len().saturating_sub(1024);
    state.data.resources.drain(..excess);
    drop(all);
    report.diagnostics = snapshot(root);
}
/// A shared report window lets entry, NIC and container workers publish the same history.
pub(super) fn snapshot(root: &Path) -> Option<NetworkDiagnostics> {
    let _save = SAVE.lock().ok()?;
    let data = {
        let mut all = registry().lock().ok()?;
        let state = runtime(&mut all, root);
        state.data.collected_at = now();
        state.data.clone()
    };
    let saved = serde_json::to_vec(&data)
        .ok()
        .is_some_and(|bytes| atomic_write_private(&path(root), &bytes).is_ok());
    let mut all = registry().lock().ok()?;
    let state = runtime(&mut all, root);
    if !saved && !state.disk_failed {
        push(
            &mut state.data,
            NetworkLogEvent {
                at: now(),
                resource_id: None,
                source: NetworkLogSource::Agent,
                level: NetworkLogLevel::Warning,
                message: "历史日志写入失败；本轮内存日志仍可查看".into(),
            },
        );
    }
    state.disk_failed = !saved;
    Some(state.data.clone())
}
fn observe(root: &Path, id: &str, phase: NetworkResourcePhase, message: String) {
    if let Ok(mut all) = registry().lock() {
        let state = runtime(&mut all, root);
        let old = state.data.resources.iter().position(|p| p.id == id);
        let changed = old.is_none_or(|i| {
            state.data.resources[i].phase != phase || state.data.resources[i].message != message
        });
        if changed {
            push(
                &mut state.data,
                NetworkLogEvent {
                    at: now(),
                    resource_id: Some(id.into()),
                    source: NetworkLogSource::Agent,
                    level: if phase == NetworkResourcePhase::Failed {
                        NetworkLogLevel::Error
                    } else {
                        NetworkLogLevel::Info
                    },
                    message: clip(&message, 1000),
                },
            );
            let progress = NetworkResourceProgress {
                id: id.into(),
                phase,
                since: now(),
                waiting_seconds: 0,
                message: clip(&message, 480),
            };
            if let Some(i) = old {
                state.data.resources[i] = progress;
            } else {
                if state.data.resources.len() >= 1024 {
                    state.data.resources.remove(0);
                }
                state.data.resources.push(progress);
            }
        }
    }
}
fn binding_summary(
    report: &NicBindingReport,
    redactions: &[String],
) -> (NetworkResourcePhase, String) {
    if let Some(error) = report.error.as_ref().or(report.local_block.as_ref()) {
        (NetworkResourcePhase::Failed, redact(error, redactions))
    } else if report.phase == BindingPhase::Withdrawing {
        if report.configured {
            (NetworkResourcePhase::Connected, "出口资源清理已确认".into())
        } else {
            (
                NetworkResourcePhase::Preparing,
                "正在清理出口资源，等待确认".into(),
            )
        }
    } else if report.phase == BindingPhase::Standby && report.configured {
        (
            NetworkResourcePhase::Connected,
            "备用出口配置已准备，等待切换".into(),
        )
    } else if !report.configured {
        (
            NetworkResourcePhase::Preparing,
            "等待应用出口网卡和路由".into(),
        )
    } else if report.reachable == Some(false) {
        (
            NetworkResourcePhase::Waiting,
            "出口配置已应用，等待对端链路".into(),
        )
    } else {
        (
            NetworkResourcePhase::Connected,
            if report.reachable == Some(true) {
                "出口配置已应用，链路可达"
            } else {
                "出口配置已应用，尚无链路观测"
            }
            .into(),
        )
    }
}
pub(super) fn bindings(root: &Path, desired: &[NicBinding], reports: &[NicBindingReport]) {
    for report in reports {
        let Some(binding) = desired.iter().find(|b| b.id == report.id) else {
            continue;
        };
        let redactions = secrets(&[binding.credential.clone(), binding.peer_credential.clone()]);
        let (phase, detail) = binding_summary(report, &redactions);
        observe(
            root,
            &report.id,
            phase,
            format!("出口配置版本 {}：{detail}", report.generation),
        );
    }
}
pub(super) fn instance(root: &Path, desired: &InstanceDesired, report: &InstanceReport) {
    let (phase, mut detail) = if let Some(error) = &report.error {
        (
            NetworkResourcePhase::Failed,
            redact(
                error,
                &secrets(&[serde_json::to_string(&desired.spec).unwrap_or_default()]),
            ),
        )
    } else {
        match report.status {
            ApplyStatus::Applied => (
                NetworkResourcePhase::Connected,
                format!(
                    "网络配置已确认，{} 张网卡，路由版本 {}",
                    report.interfaces.len(),
                    report.network.as_ref().map_or(0, |n| n.routing_revision)
                ),
            ),
            ApplyStatus::Failed => (NetworkResourcePhase::Failed, "网络配置执行失败".into()),
            _ => (
                NetworkResourcePhase::Preparing,
                "配置已提交，等待执行器确认".into(),
            ),
        }
    };
    if let Some(receipt) = &report.artifacts {
        if !receipt.mismatches.is_empty() {
            detail.push_str(&format!("；产物比对不一致：{:?}", receipt.mismatches));
        }
    }
    observe(
        root,
        &report.id,
        phase,
        format!("实例配置版本 {}：{detail}", report.generation),
    );
}

fn drain(mut reader: impl Read, mut line: impl FnMut(&str)) {
    let mut buffer = [0; 4096];
    let mut pending = Vec::new();
    let mut oversized = false;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                if oversized {
                    line("[日志行过长，已省略]");
                } else if !pending.is_empty() {
                    line(&String::from_utf8_lossy(&pending));
                }
                pending.clear();
                oversized = false;
            } else if pending.len() < 4096 {
                pending.push(*byte);
            } else {
                oversized = true;
            }
        }
    }
    if oversized {
        line("[日志行过长，已省略]");
    } else if !pending.is_empty() {
        line(&String::from_utf8_lossy(&pending));
    }
}
/// Merge stdout/stderr into one continuously drained pipe. Logging failures fail open.
pub(in crate::cvm) fn capture(
    command: &mut Command,
    root: &Path,
    id: &str,
    source: NetworkLogSource,
    redactions: Vec<String>,
) {
    let mut setup = || -> std::io::Result<_> {
        let (reader, writer) = UnixStream::pair()?;
        command
            .stdout(Stdio::from(std::os::fd::OwnedFd::from(writer.try_clone()?)))
            .stderr(Stdio::from(std::os::fd::OwnedFd::from(writer)));
        Ok(reader)
    };
    let started = setup().and_then(|reader| {
        let root = root.to_owned();
        let id = id.to_owned();
        std::thread::Builder::new()
            .name("entry-log-reader".into())
            .spawn(move || {
                drain(reader, |line| {
                    let lower = line.to_ascii_lowercase();
                    let level = if lower.contains("error") {
                        NetworkLogLevel::Error
                    } else if lower.contains("warn") {
                        NetworkLogLevel::Warning
                    } else {
                        NetworkLogLevel::Info
                    };
                    record(
                        &root,
                        Some(&id),
                        source.clone(),
                        level,
                        &redact(line, &redactions),
                    );
                });
            })
    });
    if started.is_err() {
        command.stdout(Stdio::null()).stderr(Stdio::null());
        record(
            root,
            Some(id),
            NetworkLogSource::Agent,
            NetworkLogLevel::Warning,
            "进程日志采集不可用；请检查文件描述符和线程资源",
        );
    }
}

#[cfg(test)]
mod tests;
