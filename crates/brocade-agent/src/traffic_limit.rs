//! Machine-wide NIC protection. Network delivery and local enforcement have independent clocks.
use crate::{
    fsutil::atomic_write_private, http::HttpClient, options::Options, traffic::TrafficSnapshot,
};
use brocade_deployment::{
    protocol::NodeTrafficReading,
    traffic_limit::{
        TrafficLayerObservation, TrafficLimitDesired, TrafficLimitObservation, TrafficLimitPhase,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime},
};

mod kernel;

const DESIRED: &str = "traffic-limit-desired.json";
const STATUS: &str = "traffic-limit-status.json";
const NETWORK: &str = "traffic-limit-network.json";
const IFB: &str = "bc-limit0";
const OWNER: &str = "brocade-traffic-limit";
const HANDLE: &str = "bcd0:";
const PREF: u64 = 49_152;

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let Some(bytes) = crate::fsutil::read_bounded(path, 128 * 1024)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("decode {}: {e}", path.display()))
}

fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    atomic_write_private(path, &serde_json::to_vec(value).map_err(|e| e.to_string())?)
}

pub(crate) fn spawn(options: Options, snapshot: Arc<TrafficSnapshot>) -> Result<(), String> {
    let sync_options = options.clone();
    thread::Builder::new()
        .name("traffic-limit-sync".into())
        .spawn(move || loop {
            if let Err(e) = synchronize(&sync_options) {
                eprintln!("traffic-limit sync: {e}");
            }
            thread::sleep(Duration::from_secs(15));
        })
        .map_err(|e| format!("spawn traffic limit synchronization: {e}"))?;
    thread::Builder::new()
        .name("traffic-limit".into())
        .spawn(move || loop {
            if let Err(e) = local_round(
                &options.state_dir,
                snapshot.latest(),
                &mut SystemNetwork {
                    allowed: options.apply_mode == crate::ApplyMode::Linux,
                },
            ) {
                eprintln!("traffic-limit: {e}");
            }
            thread::sleep(Duration::from_secs(10));
        })
        .map_err(|e| format!("spawn traffic limiter: {e}"))?;
    Ok(())
}

fn synchronize(options: &Options) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    // Fetch first: a failed receipt must not prevent the operator from disabling a limit.
    let response = client.request("GET", "/agent/v1/traffic-limit", &options.token, None)?;
    if response.status == 404 {
        return Ok(());
    } // New Agent, old Console: keep the saved policy.
    if response.status != 200 {
        return Err(format!(
            "traffic limit GET returned HTTP {}",
            response.status
        ));
    }
    let desired: TrafficLimitDesired =
        serde_json::from_str(&response.body).map_err(|e| e.to_string())?;
    desired.validate()?;
    let path = options.state_dir.join(DESIRED);
    // The authenticated Console remains authoritative when a local cache is corrupt. Keep
    // revision rollback protection for readable caches without making damage irreversible.
    let previous: Option<TrafficLimitDesired> = match read_json(&path) {
        Ok(previous) => previous,
        Err(error) => {
            eprintln!("traffic-limit: replacing unreadable policy cache from Console: {error}");
            None
        }
    };
    if previous
        .as_ref()
        .is_some_and(|p| desired.revision < p.revision)
    {
        return Err("traffic limit revision regressed; retaining durable policy".into());
    }
    if previous.as_ref() != Some(&desired) {
        save(&path, &desired)?;
    }
    if let Some(status) = recent_status(&options.state_dir, SystemTime::now())? {
        let body = serde_json::to_string(&status).map_err(|e| e.to_string())?;
        let response = client.request(
            "POST",
            "/agent/v1/traffic-limit",
            &options.token,
            Some(&body),
        )?;
        if response.status != 204 {
            return Err(format!(
                "traffic limit receipt returned HTTP {}",
                response.status
            ));
        }
    }
    Ok(())
}

fn recent_status(dir: &Path, now: SystemTime) -> Result<Option<TrafficLimitObservation>, String> {
    let path = dir.join(STATUS);
    let modified = match fs::metadata(&path) {
        Ok(metadata) => metadata.modified().map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    // A healthy synchronization worker is not evidence that the local enforcement worker is
    // still making progress. Do not refresh the Console's freshness with an old status file.
    if now.duration_since(modified).unwrap_or(Duration::MAX) > Duration::from_secs(45) {
        return Ok(None);
    }
    read_json(&path)
}

/// Same directional anchor as Store, including max-of-directions and calibration semantics.
fn used_bytes(
    desired: &TrafficLimitDesired,
    reading: &NodeTrafficReading,
    now: i64,
) -> Result<(u64, i64), String> {
    let (period, _) = desired.period(now);
    let anchor = desired
        .anchor
        .as_ref()
        .ok_or("等待控制面确认流量计量基线")?;
    if anchor.meter_id != reading.meter_id || reading.sequence < anchor.sequence {
        return Err("流量计量基线已变化，等待重新同步".into());
    }
    let (base_rx, base_tx, meter_rx, meter_tx, calibrated) =
        if anchor.period_start_unix_secs == period && anchor.sampled_at_unix_secs >= period {
            (
                anchor.rx_bytes,
                anchor.tx_bytes,
                anchor.meter_rx_bytes,
                anchor.meter_tx_bytes,
                anchor.calibrated_total_bytes,
            )
        } else {
            let boundary = reading
                .day_boundaries
                .iter()
                .find(|b| b.day_start_unix_secs == period)
                .ok_or("缺少本账期重置边界，保持已有上限并等待计量同步")?;
            (
                0,
                0,
                boundary.rx_bytes,
                boundary.tx_bytes,
                if anchor.period_start_unix_secs == period {
                    anchor.calibrated_total_bytes
                } else {
                    0
                },
            )
        };
    let rx = reading
        .rx_bytes
        .checked_sub(meter_rx)
        .and_then(|v| base_rx.checked_add(v))
        .ok_or("接收计数倒退或溢出")?;
    let tx = reading
        .tx_bytes
        .checked_sub(meter_tx)
        .and_then(|v| base_tx.checked_add(v))
        .ok_or("发送计数倒退或溢出")?;
    let used = calibrated
        .checked_add(desired.accounting_mode.apply(rx, tx))
        .ok_or("账期总量溢出")?;
    Ok((used, period))
}

fn layer_result(
    result: Result<(), String>,
    rate: Option<u64>,
    enabled: bool,
) -> TrafficLayerObservation {
    match result {
        Ok(()) => TrafficLayerObservation {
            phase: if !enabled {
                TrafficLimitPhase::Disabled
            } else if rate.is_some() {
                TrafficLimitPhase::Limited
            } else {
                TrafficLimitPhase::Ready
            },
            rate_bps: rate,
            error: None,
        },
        Err(error) => TrafficLayerObservation {
            phase: TrafficLimitPhase::Error,
            rate_bps: None,
            error: Some(error.chars().take(500).collect()),
        },
    }
}

pub(crate) fn apply_application(
    dir: &Path,
    enabled: bool,
    rate: Option<u64>,
) -> Result<(), String> {
    crate::traffic_limit_budget::ensure_configured(dir, enabled, rate)?;
    if !enabled {
        return Ok(());
    }
    let config =
        fs::read_to_string(dir.join("xray.json")).map_err(|_| "等待 Xray 启动以确认应用层限速")?;
    let api = crate::xray_api_address(&config).ok_or("Xray 本地 API 地址不可用")?;
    let _deadline = crate::xray_grpc::deadline_after(Some(Duration::from_secs(2)));
    let stats = crate::xray_grpc::query_stats(api, crate::traffic_limit_budget::STATS_NAME)
        .map_err(|_| "应用层限速未确认，需要支持机器限速的受管 Xray 及可用本地 API")?;
    if !stats.iter().any(|s| {
        s.name == crate::traffic_limit_budget::STATS_NAME
            && u64::try_from(s.value).ok() == Some(rate.unwrap_or(0))
    }) {
        return Err(
            "Xray 尚未确认应用层上限；请升级支持机器限速的受管 Xray，并重启使能力生效".into(),
        );
    }
    confirm_application_generations(dir)
}

fn generation_supports_limit(
    process: &brocade_deployment::protocol::XrayUpgradeProcess,
) -> Result<bool, String> {
    generation_supports_capability(process, "machine-traffic-limit-v1")
}
pub(crate) fn generation_supports_capability(
    process: &brocade_deployment::protocol::XrayUpgradeProcess,
    capability: &'static str,
) -> Result<bool, String> {
    use std::os::fd::AsRawFd;
    static CAPABILITIES: Mutex<Vec<(String, &'static str, bool)>> = Mutex::new(Vec::new());
    if let Some((_, _, supported)) = CAPABILITIES
        .lock()
        .map_err(|_| "Xray 能力缓存锁损坏")?
        .iter()
        .find(|(sha, cap, _)| sha == &process.sha256 && *cap == capability)
    {
        return Ok(*supported);
    }
    // Pin the verified executable inode while running version. A reused PID cannot make
    // a privileged capability probe execute another process's program.
    let executable = fs::File::open(format!("/proc/{}/exe", process.pid))
        .map_err(|_| "无法读取 Xray 代际可执行文件")?;
    let pinned = std::path::PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        executable.as_raw_fd()
    ));
    if crate::cached_xray_sha256(&pinned)? != process.sha256 {
        return Err("Xray 代际可执行文件身份已变化".into());
    }
    let output = crate::command::run_command_with_timeout(
        pinned.to_str().ok_or("无效 Xray 可执行路径")?,
        &["version"],
        Duration::from_secs(2),
    )
    .map_err(|_| "Xray 代际限速能力未能验证")?;
    drop(executable);
    let supported = crate::xray_version_has_capability(&output, capability);
    let mut cache = CAPABILITIES.lock().map_err(|_| "Xray 能力缓存锁损坏")?;
    if cache.len() >= 8 {
        cache.remove(0);
    }
    cache.push((process.sha256.clone(), capability, supported));
    Ok(supported)
}

fn confirm_application_generations(dir: &Path) -> Result<(), String> {
    let Some(runtime) = crate::xrayupdate::read_upgrade_runtime(dir)? else {
        return Ok(());
    };
    let expected = crate::traffic_limit_budget::control_path(
        &fs::canonicalize(dir).map_err(|_| "限速状态目录不可用")?,
    );
    let mut live = 0;
    for process in std::iter::once(&runtime.old)
        .chain(runtime.candidate.as_ref())
        .filter(|p| p.alive)
    {
        live += 1;
        if !generation_supports_limit(process)? {
            return Err(format!(
                "Xray 进程 {} 尚不支持机器限速，等待旧代排空后再确认应用层上限",
                process.pid
            ));
        }
        let identity_matches = || {
            crate::process_ref(process.pid)
                .is_some_and(|p| p.start_ticks == process.pid_start_ticks)
        };
        if !identity_matches() {
            return Err("Xray 代际身份已变化，等待重新确认".into());
        }
        let environment = crate::fsutil::read_bounded(
            &std::path::PathBuf::from(format!("/proc/{}/environ", process.pid)),
            64 * 1024,
        )?
        .ok_or("Xray 代际环境暂不可用")?;
        let shared = environment.split(|b| *b == 0).any(|entry| {
            entry
                .strip_prefix(b"XRAY_BROCADE_MACHINE_LIMIT_FILE=")
                .is_some_and(|path| path == expected.as_os_str().as_encoded_bytes())
        });
        if !identity_matches() || !shared {
            return Err(format!(
                "Xray 进程 {} 尚未接入整机共享预算，等待旧代排空后再确认应用层上限",
                process.pid
            ));
        }
    }
    if live == 0 {
        return Err("Xray 代际状态尚未收敛，等待应用层确认".into());
    }
    Ok(())
}

fn local_round(
    dir: &Path,
    reading: Option<NodeTrafficReading>,
    network: &mut impl Network,
) -> Result<(), String> {
    let Some(desired) = read_json::<TrafficLimitDesired>(&dir.join(DESIRED))? else {
        return Ok(());
    };
    desired.validate()?;
    let mut report = TrafficLimitObservation {
        revision: desired.revision,
        phase: TrafficLimitPhase::Disabled,
        interface: reading.as_ref().map(|r| r.interface.clone()),
        rate_bps: None,
        used_bytes: None,
        period_start_unix_secs: None,
        error: None,
        system: None,
        application: None,
    };
    let computed = if desired.policy.enabled {
        reading
            .as_ref()
            .ok_or_else(|| "等待有效网卡计量读数".to_owned())
            .and_then(|r| used_bytes(&desired, r, crate::current_unix_secs()?))
            .map(Some)
    } else {
        Ok(None)
    };
    let result = match computed {
        Ok(usage) => {
            if let Some((used, period)) = usage {
                report.used_bytes = Some(used.to_string());
                report.period_start_unix_secs = Some(period);
                report.rate_bps = desired.policy.rate(used, desired.quota_bytes);
            }
            let system = desired.policy.enabled && desired.policy.layer.system();
            let application = desired.policy.enabled && desired.policy.layer.application();
            let system_result = layer_result(
                reconcile_network(
                    dir,
                    reading.as_ref().map(|r| r.interface.as_str()),
                    report.rate_bps.filter(|_| system),
                    network,
                ),
                report.rate_bps.filter(|_| system),
                system,
            );
            // Execute independently: an unavailable kernel must not prevent application protection.
            let application_result = layer_result(
                apply_application(dir, application, report.rate_bps.filter(|_| application)),
                report.rate_bps.filter(|_| application),
                application,
            );
            let errors: Vec<_> = [
                system_result.error.as_deref(),
                application_result.error.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect();
            let error = (!errors.is_empty()).then(|| errors.join("；"));
            report.system = Some(system_result);
            report.application = Some(application_result);
            if let Some(error) = error {
                Err(error)
            } else {
                Ok(())
            }
        }
        Err(error) => {
            // Invalid/missing counters never silently remove the existing cap. The loopback
            // application budget is already restored before Xray startup, independently of this read.
            match read_json::<AppliedNetwork>(&dir.join(NETWORK)) {
                Ok(Some(saved)) => {
                    report.rate_bps = Some(saved.rate_bps);
                    report.system = Some(layer_result(
                        reconcile_network(
                            dir,
                            Some(&saved.interface),
                            Some(saved.rate_bps),
                            network,
                        ),
                        Some(saved.rate_bps),
                        true,
                    ));
                }
                Ok(None) => {}
                Err(error) => report.system = Some(layer_result(Err(error), None, true)),
            }
            Err(error)
        }
    };
    report.phase = if result.is_err() {
        if desired.policy.enabled && desired.anchor.is_none() {
            TrafficLimitPhase::Waiting
        } else {
            TrafficLimitPhase::Error
        }
    } else if !desired.policy.enabled {
        TrafficLimitPhase::Disabled
    } else if report.rate_bps.is_some() {
        TrafficLimitPhase::Limited
    } else {
        TrafficLimitPhase::Ready
    };
    if let Err(error) = &result {
        report.error = Some(error.chars().take(500).collect());
    }
    save(&dir.join(STATUS), &report)?;
    result
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppliedNetwork {
    interface: String,
    previous_root: String,
    rate_bps: u64,
}

trait Network {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String>;
    fn classes(&mut self, interface: &str) -> Result<Vec<kernel::Class>, String> {
        kernel::classes(interface)
    }
}
struct SystemNetwork {
    allowed: bool,
}
impl Network for SystemNetwork {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
        if !self.allowed {
            return Err("state-dir 模式不执行系统网卡限速".into());
        }
        crate::command::run_command_with_timeout(program, args, Duration::from_secs(5))
    }
}

fn json(network: &mut impl Network, program: &str, args: &[&str]) -> Result<Vec<Value>, String> {
    let output = network.run(program, args)?;
    // iproute2 emits no bytes (rather than []) for an empty class/filter listing.
    if program == "tc" && output.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&output).map_err(|e| format!("invalid {program} JSON: {e}"))
}

fn qdiscs(network: &mut impl Network, interface: &str) -> Result<Vec<Value>, String> {
    json(network, "tc", &["-j", "qdisc", "show", "dev", interface])
}
fn owned_root(q: &Value) -> bool {
    q["root"] == true && q["handle"] == HANDLE && q["kind"] == "htb"
}
fn correct_default(q: &Value) -> bool {
    q["options"]["default"] == "0x10" || q["options"]["default"] == 16
}
fn valid_interface(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 15
        && !value.starts_with('-')
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

fn reconcile_network(
    dir: &Path,
    interface: Option<&str>,
    rate: Option<u64>,
    network: &mut impl Network,
) -> Result<(), String> {
    let mut saved: Option<AppliedNetwork> = read_json(&dir.join(NETWORK))?;
    if let Some(old) = saved
        .as_ref()
        .filter(|s| rate.is_none() || Some(s.interface.as_str()) != interface)
    {
        cleanup(old, network)?;
        fs::remove_file(dir.join(NETWORK)).map_err(|e| e.to_string())?;
        saved = None;
    }
    let Some(rate) = rate else {
        return Ok(());
    };
    if !(1_000..=brocade_deployment::traffic_limit::MAX_TRAFFIC_RATE_BPS).contains(&rate) {
        return Err("无效限速值".into());
    }
    let interface = interface
        .filter(|s| valid_interface(s) && *s != IFB)
        .ok_or("无效物理网卡名称")?;
    let existing = qdiscs(network, interface)?;
    let root = existing
        .iter()
        .find(|q| q["root"] == true)
        .ok_or("无法识别网卡根队列")?;
    let previous_root = if owned_root(root) {
        saved
            .as_ref()
            .map(|s| s.previous_root.clone())
            .ok_or("限速队列存在但缺少所有权检查点")?
    } else {
        let kind = root["kind"].as_str().ok_or("缺少根队列类型")?;
        let handle = root["handle"].as_str().unwrap_or("");
        // Only kernel-assigned default queues may be adopted. Custom handles/QoS need an operator.
        if !matches!(kind, "fq" | "fq_codel" | "pfifo_fast" | "mq" | "noqueue")
            || !(handle == "0:" || handle.starts_with("80"))
        {
            return Err("网卡已有自定义 QoS，拒绝覆盖".into());
        }
        saved
            .as_ref()
            .map(|s| s.previous_root.clone())
            .unwrap_or_else(|| kind.to_owned())
    };
    let links = json(network, "ip", &["-j", "-d", "link", "show"])?;
    if let Some(ifb) = links.iter().find(|l| l["ifname"] == IFB) {
        if ifb["linkinfo"]["info_kind"] != "ifb"
            || (ifb["ifalias"] != OWNER
                && !(saved.is_some() && ifb["ifalias"].as_str().unwrap_or("").is_empty()))
        {
            return Err("限速 IFB 名称已被其他配置占用".into());
        }
    }
    let filters = if existing
        .iter()
        .any(|q| matches!(q["kind"].as_str(), Some("clsact" | "ingress")))
    {
        json(
            network,
            "tc",
            &["-j", "filter", "show", "dev", interface, "ingress"],
        )?
    } else {
        Vec::new()
    };
    if !exclusive_ingress(&filters) {
        return Err("网卡已有自定义入站过滤，拒绝覆盖或绕过".into());
    }
    // Durable intent precedes every kernel mutation, including a partially created IFB.
    let wanted = AppliedNetwork {
        interface: interface.into(),
        previous_root,
        rate_bps: rate,
    };
    save(&dir.join(NETWORK), &wanted)?;
    if !links.iter().any(|l| l["ifname"] == IFB)
        && network
            .run("ip", &["link", "add", IFB, "type", "ifb"])
            .is_err()
    {
        network.run("modprobe", &["ifb"])?;
        network.run("ip", &["link", "add", IFB, "type", "ifb"])?;
    }
    network.run("ip", &["link", "set", "dev", IFB, "alias", OWNER])?;
    network.run("ip", &["link", "set", "dev", IFB, "up"])?;
    ensure_shaper(network, interface, rate)?;
    ensure_shaper(network, IFB, rate)?;
    if !existing
        .iter()
        .any(|q| matches!(q["kind"].as_str(), Some("clsact" | "ingress")))
    {
        network.run("tc", &["qdisc", "add", "dev", interface, "clsact"])?;
    }
    if !filters.iter().any(owned_filter) {
        network.run(
            "tc",
            &[
                "filter", "replace", "dev", interface, "ingress", "protocol", "all", "pref",
                "49152", "handle", "0xbcd0", "matchall", "action", "mirred", "egress", "redirect",
                "dev", IFB,
            ],
        )?;
    }
    // A successful command is insufficient: verify both aggregate caps and the ingress redirect.
    verify_shaper(network, interface, rate)?;
    verify_shaper(network, IFB, rate)?;
    let filters = json(
        network,
        "tc",
        &["-j", "filter", "show", "dev", interface, "ingress"],
    )?;
    if !filters.iter().any(owned_filter) || !exclusive_ingress(&filters) {
        return Err("入站限速过滤器回读不一致".into());
    }
    Ok(())
}

fn exclusive_ingress(filters: &[Value]) -> bool {
    filters.is_empty()
        || (filters.iter().any(owned_filter)
            && filters.iter().all(|f| {
                f["pref"].as_u64() == Some(PREF)
                    && f["kind"] == "matchall"
                    && (f["options"].is_null() || owned_filter(f))
            }))
}

fn owned_filter(filter: &Value) -> bool {
    filter["pref"].as_u64() == Some(PREF)
        && filter["kind"] == "matchall"
        && filter["chain"].as_u64().unwrap_or(0) == 0
        && filter["options"]["handle"].as_u64() == Some(0xbcd0)
        && filter["options"]["actions"]
            .as_array()
            .is_some_and(|actions| {
                actions.len() == 1
                    && actions[0]["kind"] == "mirred"
                    && actions[0]["to_dev"] == IFB
                    && actions[0]["mirred_action"] == "redirect"
                    && actions[0]["direction"] == "egress"
            })
}

fn ensure_shaper(network: &mut impl Network, interface: &str, rate: u64) -> Result<(), String> {
    let mut queues = qdiscs(network, interface)?;
    if queues.iter().any(|q| owned_root(q) && !correct_default(q)) {
        // Some kernels reject changing an HTB root's default class in place. Its ownership
        // intent is already durable, so recreate only our damaged root and all its children.
        network.run("tc", &["qdisc", "del", "dev", interface, "root"])?;
        queues = qdiscs(network, interface)?;
    }
    if !queues.iter().any(owned_root) {
        if interface == IFB
            && queues.iter().any(|q| {
                q["root"] == true
                    && !(matches!(
                        q["kind"].as_str(),
                        Some("noqueue" | "fq" | "fq_codel" | "pfifo_fast")
                    ) && q["handle"] == "0:")
            })
        {
            return Err("限速 IFB 存在非受管根队列".into());
        }
        network.run(
            "tc",
            &[
                "qdisc", "replace", "dev", interface, "root", "handle", HANDLE, "htb", "default",
                "10",
            ],
        )?;
    }
    let classes = network.classes(interface)?;
    if !classes.iter().any(|c| class_matches(c, rate)) {
        let speed = format!("{rate}bit");
        network.run(
            "tc",
            &[
                "class", "replace", "dev", interface, "parent", HANDLE, "classid", "bcd0:10",
                "htb", "rate", &speed, "ceil", &speed, "quantum", "1514",
            ],
        )?;
    }
    if !queues
        .iter()
        .any(|q| q["parent"] == "bcd0:10" && q["kind"] == "fq")
    {
        network.run(
            "tc",
            &[
                "qdisc",
                "replace",
                "dev",
                interface,
                "parent",
                "bcd0:10",
                "handle",
                "bcd1:",
                "fq",
                "limit",
                "1024",
                "flow_limit",
                "64",
            ],
        )?;
    }
    Ok(())
}

fn class_matches(class: &kernel::Class, rate: u64) -> bool {
    class.handle == 0xbcd0_0010
        && class.kind == "htb"
        && class.rate == Some(rate / 8)
        && class.ceil == Some(rate / 8)
}

fn verify_shaper(network: &mut impl Network, interface: &str, rate: u64) -> Result<(), String> {
    let queues = qdiscs(network, interface)?;
    let classes = network.classes(interface)?;
    if !queues.iter().any(|q| owned_root(q) && correct_default(q))
        || !queues
            .iter()
            .any(|q| q["parent"] == "bcd0:10" && q["kind"] == "fq")
        || classes.len() != 1
        || !class_matches(&classes[0], rate)
    {
        return Err(format!("{interface} 限速上限回读不一致"));
    }
    Ok(())
}

fn cleanup(saved: &AppliedNetwork, network: &mut impl Network) -> Result<(), String> {
    if !valid_interface(&saved.interface)
        || !matches!(
            saved.previous_root.as_str(),
            "fq" | "fq_codel" | "pfifo_fast" | "mq" | "noqueue"
        )
    {
        return Err("无效限速所有权检查点".into());
    }
    let links = json(network, "ip", &["-j", "-d", "link", "show"])?;
    if links.iter().any(|l| l["ifname"] == saved.interface) {
        let filters = json(
            network,
            "tc",
            &["-j", "filter", "show", "dev", &saved.interface, "ingress"],
        )?;
        if filters.iter().any(owned_filter) {
            network.run(
                "tc",
                &[
                    "filter",
                    "del",
                    "dev",
                    &saved.interface,
                    "ingress",
                    "protocol",
                    "all",
                    "pref",
                    "49152",
                    "handle",
                    "0xbcd0",
                    "matchall",
                ],
            )?;
        }
        if qdiscs(network, &saved.interface)?.iter().any(owned_root) {
            if matches!(saved.previous_root.as_str(), "mq" | "noqueue") {
                network.run("tc", &["qdisc", "del", "dev", &saved.interface, "root"])?;
            } else {
                network.run(
                    "tc",
                    &[
                        "qdisc",
                        "replace",
                        "dev",
                        &saved.interface,
                        "root",
                        &saved.previous_root,
                    ],
                )?;
            }
        }
    }
    if let Some(ifb) = links.iter().find(|l| l["ifname"] == IFB) {
        if ifb["linkinfo"]["info_kind"] != "ifb"
            || (ifb["ifalias"] != OWNER && !ifb["ifalias"].as_str().unwrap_or("").is_empty())
        {
            return Err("拒绝删除其他配置占用的 IFB".into());
        }
        network.run("ip", &["link", "del", "dev", IFB])?;
    }
    Ok(())
}

/// Reinstall the durable last cap before any serving process is restored on boot.
pub(crate) fn restore_saved_network(dir: &Path) -> Result<(), String> {
    if let Some(saved) = read_json::<AppliedNetwork>(&dir.join(NETWORK))? {
        reconcile_network(
            dir,
            Some(&saved.interface),
            Some(saved.rate_bps),
            &mut SystemNetwork { allowed: true },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traffic_limit_generation_metadata_fifo_is_rejected_without_blocking() {
        let dir = std::env::temp_dir().join(format!(
            "brocade-limit-generation-fifo-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = std::ffi::CString::new(
            dir.join(crate::xrayupdate::UPGRADE_RUNTIME_FILE)
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        // SAFETY: path is a NUL-terminated temporary pathname and the mode is valid.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(crate::xrayupdate::read_upgrade_runtime(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn traffic_limit_corrupt_desired_recovers_from_authoritative_delivery() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "brocade-limit-corrupt-desired-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(DESIRED), b"{truncated").unwrap();
        let mut desired = desired();
        desired.policy = TrafficLimitPolicy::default();
        let expected = desired.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut stream);
            loop {
                let mut line = String::new();
                assert!(std::io::BufRead::read_line(&mut reader, &mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            let body = serde_json::to_vec(&desired).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        let options = Options {
            command: "run".into(),
            server: format!("http://{endpoint}"),
            token: "test-token".into(),
            state_dir: dir.clone(),
            apply_mode: crate::ApplyMode::StateDir,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let result = synchronize(&options);
        server.join().unwrap();
        assert!(
            result.is_ok(),
            "authoritative policy must repair a corrupt cache: {result:?}"
        );
        assert_eq!(
            read_json::<TrafficLimitDesired>(&dir.join(DESIRED)).unwrap(),
            Some(expected)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn traffic_limit_corrupt_network_checkpoint_does_not_block_application_disable() {
        struct NoNetwork;
        impl Network for NoNetwork {
            fn run(&mut self, _: &str, _: &[&str]) -> Result<String, String> {
                panic!("corrupt ownership must never cause network mutations");
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "brocade-limit-corrupt-network-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut desired = desired();
        desired.anchor = None;
        save(&dir.join(DESIRED), &desired).unwrap();
        fs::write(dir.join(NETWORK), b"{truncated").unwrap();
        assert!(local_round(&dir, None, &mut NoNetwork).is_err());
        let report = read_json::<TrafficLimitObservation>(&dir.join(STATUS))
            .unwrap()
            .expect("system errors must still publish fresh layer results");
        assert_eq!(report.phase, TrafficLimitPhase::Error);
        assert_eq!(report.system.unwrap().phase, TrafficLimitPhase::Error);
        assert_eq!(
            report.application.unwrap().phase,
            TrafficLimitPhase::Disabled
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn traffic_limit_stale_enforcement_status_is_not_republished_as_fresh() {
        let dir =
            std::env::temp_dir().join(format!("brocade-limit-receipt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(recent_status(&dir, SystemTime::now()).unwrap().is_none());
        let report = TrafficLimitObservation {
            revision: 1,
            phase: TrafficLimitPhase::Limited,
            interface: Some("eth0".into()),
            rate_bps: Some(100_000_000),
            used_bytes: Some("100".into()),
            period_start_unix_secs: None,
            error: None,
            system: None,
            application: None,
        };
        save(&dir.join(STATUS), &report).unwrap();
        let now = SystemTime::now();
        assert_eq!(recent_status(&dir, now).unwrap(), Some(report));
        assert!(recent_status(&dir, now + Duration::from_secs(46))
            .unwrap()
            .is_none());
        fs::remove_dir_all(dir).unwrap();
    }
    use brocade_deployment::traffic_limit::*;
    fn desired() -> TrafficLimitDesired {
        TrafficLimitDesired {
            revision: 1,
            policy: TrafficLimitPolicy::default(),
            quota_bytes: Some(1000),
            cycle_kind: NodeTrafficCycleKind::Monthly,
            accounting_mode: NodeTrafficAccountingMode::BidirectionalMax,
            reset_month: None,
            reset_day: 1,
            anchor: Some(TrafficLimitAnchor {
                meter_id: "meter".into(),
                sequence: 1,
                sampled_at_unix_secs: 1_759_276_800,
                period_start_unix_secs: 1_759_276_800,
                meter_rx_bytes: 1000,
                meter_tx_bytes: 2000,
                rx_bytes: 50,
                tx_bytes: 100,
                calibrated_total_bytes: 500,
            }),
        }
    }
    fn reading() -> NodeTrafficReading {
        NodeTrafficReading {
            meter_id: "meter".into(),
            sequence: 2,
            interface: "eth0".into(),
            boot_id: "boot".into(),
            rx_bytes: 1100,
            tx_bytes: 2010,
            discontinuities: 0,
            sampled_at_unix_secs: Some(1_759_276_810),
            day_boundaries: vec![],
        }
    }
    #[test]
    fn traffic_limit_preserves_directional_max_and_calibration() {
        let d = desired();
        let r = reading();
        assert_eq!(used_bytes(&d, &r, 1_759_276_810).unwrap().0, 650);
        let mut replaced = r;
        replaced.meter_id = "replacement".into();
        assert!(used_bytes(&d, &replaced, 1_759_276_810).is_err());
    }
    #[test]
    fn traffic_limit_offline_reset_uses_durable_boundary() {
        let d = desired();
        let mut r = reading();
        let next_period = d.period(1_759_276_810).1;
        r.day_boundaries.push(TrafficDayBoundary {
            day_start_unix_secs: next_period,
            rx_bytes: 1050,
            tx_bytes: 2005,
        });
        assert_eq!(
            used_bytes(&d, &r, next_period + 10).unwrap(),
            (50, next_period)
        );
        r.day_boundaries.clear();
        assert!(used_bytes(&d, &r, next_period + 10).is_err());
    }
    #[test]
    fn traffic_limit_kernel_verification_requires_aggregate_rate_and_owned_redirect() {
        let class = kernel::Class {
            handle: 0xbcd0_0010,
            kind: "htb".into(),
            rate: Some(12_500_000),
            ceil: Some(12_500_000),
        };
        assert!(class_matches(&class, 100_000_000));
        for invalid in [
            kernel::Class {
                rate: None,
                ..class.clone()
            },
            kernel::Class {
                ceil: None,
                ..class.clone()
            },
            kernel::Class {
                rate: Some(125_000_000),
                ..class.clone()
            },
            kernel::Class {
                ceil: Some(125_000_000),
                ..class.clone()
            },
            kernel::Class {
                kind: "hfsc".into(),
                ..class.clone()
            },
            kernel::Class {
                handle: 0x1234_0010,
                ..class.clone()
            },
        ] {
            assert!(!class_matches(&invalid, 100_000_000));
        }
        assert!(owned_filter(
            &serde_json::json!({"pref":49152,"kind":"matchall","options":{"handle":48336,"actions":[{"kind":"mirred","to_dev":"bc-limit0","mirred_action":"redirect","direction":"egress"}]}})
        ));
        for interface in ["eth0;reboot", "../eth0", "-eth0", "eth0\n"] {
            assert!(!valid_interface(interface));
        }
    }
    #[test]
    fn traffic_limit_verified_kernel_class_does_not_reapply() {
        struct ReadOnlyNetwork;
        impl Network for ReadOnlyNetwork {
            fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
                match (program, args) {
                    ("tc", ["-j", "qdisc", "show", "dev", "eth0"]) => Ok(
                        r#"[{"kind":"htb","handle":"bcd0:","root":true,"options":{"default":"0x10"}},{"kind":"fq","handle":"bcd1:","parent":"bcd0:10"}]"#.into(),
                    ),
                    _ => Err(format!("unexpected network mutation: {program} {args:?}")),
                }
            }
            fn classes(&mut self, _: &str) -> Result<Vec<kernel::Class>, String> {
                Ok(vec![kernel::Class {
                    handle: 0xbcd0_0010,
                    kind: "htb".into(),
                    rate: Some(12_500_000),
                    ceil: Some(12_500_000),
                }])
            }
        }
        let mut network = ReadOnlyNetwork;
        verify_shaper(&mut network, "eth0", 100_000_000).unwrap();
        ensure_shaper(&mut network, "eth0", 100_000_000).unwrap();
        assert!(verify_shaper(&mut network, "eth0", 30_000_000).is_err());
    }
    #[test]
    #[ignore = "runs inside the explicit Docker network fixture"]
    fn traffic_limit_netlink_guest() {
        let interface = std::env::var("BROCADE_HTB_INTERFACE").expect("explicit fixture interface");
        println!(
            "HTB_CLASSES={}",
            serde_json::to_string(&kernel::classes(&interface).unwrap()).unwrap()
        );
    }
    #[test]
    #[ignore = "requires Docker and an image with iproute2; BROCADE_TRAFFIC_LIMIT_NET_IMAGE"]
    fn traffic_limit_system_real_kernel_reconciles_caps_and_preserves_external_qos() {
        struct DockerNetwork(String);
        impl Network for DockerNetwork {
            fn run(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
                let mut command = vec!["exec", self.0.as_str(), program];
                command.extend_from_slice(args);
                crate::command::run_command_with_timeout(
                    "docker",
                    &command,
                    Duration::from_secs(10),
                )
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
        let image =
            std::env::var("BROCADE_TRAFFIC_LIMIT_NET_IMAGE").expect("explicit network test image");
        let executable = std::env::current_exe().unwrap();
        let mount = format!("{}:/brocade-test:ro", executable.display());
        let id = crate::run_command(
            "docker",
            &[
                "run",
                "-d",
                "--volume",
                &mount,
                "--network",
                "none",
                "--cap-add",
                "NET_ADMIN",
                &image,
                "sleep",
                "120",
            ],
        )
        .unwrap();
        let mut network = DockerNetwork(id.trim().to_owned());
        network
            .run("ip", &["link", "add", "limit-test0", "type", "dummy"])
            .unwrap();
        network
            .run("ip", &["link", "set", "limit-test0", "up"])
            .unwrap();
        let dir = std::env::temp_dir().join(format!("brocade-limit-net-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for rate in [
            100_000_000,
            512_000,
            1_234_567_000,
            40_000_001_000,
            1_000_000_000_000,
        ] {
            reconcile_network(&dir, Some("limit-test0"), Some(rate), &mut network).unwrap();
            verify_shaper(&mut network, "limit-test0", rate).unwrap();
            verify_shaper(&mut network, IFB, rate).unwrap();
            // Repeated rounds must retain the queue and continue satisfying both caps.
            reconcile_network(&dir, Some("limit-test0"), Some(rate), &mut network).unwrap();
        }
        network
            .run("tc", &["qdisc", "del", "dev", "limit-test0", "root"])
            .unwrap();
        network
            .run(
                "tc",
                &[
                    "qdisc",
                    "add",
                    "dev",
                    "limit-test0",
                    "root",
                    "handle",
                    HANDLE,
                    "htb",
                    "default",
                    "20",
                ],
            )
            .unwrap();
        assert!(verify_shaper(&mut network, "limit-test0", 512_000).is_err());
        reconcile_network(&dir, Some("limit-test0"), Some(512_000), &mut network)
            .expect("owned root default drift must be repaired");
        verify_shaper(&mut network, "limit-test0", 512_000).unwrap();
        network
            .run(
                "tc",
                &[
                    "filter",
                    "add",
                    "dev",
                    "limit-test0",
                    "ingress",
                    "pref",
                    "1",
                    "matchall",
                    "action",
                    "pass",
                ],
            )
            .unwrap();
        assert!(
            reconcile_network(&dir, Some("limit-test0"), Some(100_000_000), &mut network).is_err()
        );
        network
            .run(
                "tc",
                &[
                    "filter",
                    "del",
                    "dev",
                    "limit-test0",
                    "ingress",
                    "pref",
                    "1",
                ],
            )
            .unwrap();
        network
            .run(
                "tc",
                &[
                    "qdisc",
                    "replace",
                    "dev",
                    "limit-test0",
                    "root",
                    "handle",
                    "1234:",
                    "tbf",
                    "rate",
                    "1mbit",
                    "burst",
                    "32k",
                    "latency",
                    "50ms",
                ],
            )
            .unwrap();
        assert!(
            reconcile_network(&dir, Some("limit-test0"), Some(100_000_000), &mut network).is_err()
        );
        reconcile_network(&dir, None, None, &mut network).unwrap();
        assert!(qdiscs(&mut network, "limit-test0")
            .unwrap()
            .iter()
            .any(|q| q["handle"] == "1234:"));
        assert!(!json(&mut network, "ip", &["-j", "-d", "link", "show"])
            .unwrap()
            .iter()
            .any(|l| l["ifname"] == IFB));
        fs::remove_dir_all(dir).unwrap();
    }
}
