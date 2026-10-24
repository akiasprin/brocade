//! Durable source admission shared by serving and draining Xray generations.
use crate::{
    fsutil::{atomic_write_private, read_bounded},
    http::HttpClient,
    options::Options,
};
use brocade_deployment::source_access::*;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

pub(crate) const ENV: &str = "XRAY_BROCADE_SOURCE_ACCESS_FILE";
const MAX_FILE: usize = MAX_SOURCE_ACCESS_BYTES;
pub(crate) fn policy_path(dir: &Path) -> PathBuf {
    dir.join("source-access-policy.json")
}

fn saved(dir: &Path) -> Result<Option<SourceAccessDesired>, String> {
    read_bounded(&policy_path(dir), MAX_FILE)?
        .map(|bytes| {
            let desired: SourceAccessDesired =
                serde_json::from_slice(&bytes).map_err(|_| "invalid source access checkpoint")?;
            desired.validate()?;
            Ok(desired)
        })
        .transpose()
}
pub(crate) fn initialize(dir: &Path) -> Result<(), String> {
    if read_bounded(&policy_path(dir), MAX_FILE)?.is_none() {
        // A provisioned node may have lost its checkpoint. Only an authenticated
        // Console snapshot may replace that unknown state with an empty policy.
        if dir.join("xray.json").exists() {
            return Err("来源策略检查点缺失，等待 Console 恢复后再启动 Xray".into());
        }
        atomic_write_private(
            &policy_path(dir),
            &serde_json::to_vec(&SourceAccessDesired::default()).map_err(|e| e.to_string())?,
        )?;
    }
    Ok(())
}
fn persist(dir: &Path, desired: &SourceAccessDesired) -> Result<(), String> {
    desired.validate()?;
    // Corrupt cache can be repaired only by an authenticated authoritative snapshot.
    if let Ok(Some(previous)) = saved(dir) {
        if previous.revision > desired.revision {
            return Err("source access revision regressed; retaining saved denials".into());
        }
        if previous == *desired {
            return Ok(());
        }
    }
    atomic_write_private(
        &policy_path(dir),
        &serde_json::to_vec(desired).map_err(|e| e.to_string())?,
    )
}

pub(crate) fn spawn(options: Options) -> Result<(), String> {
    if let Err(error) = initialize(&options.state_dir) {
        eprintln!("source access checkpoint: {error}");
    }
    // Fetch before starting Xray. Offline operation keeps the durable last-known policy.
    if let Err(error) = synchronize(&options, true) {
        eprintln!("source access initial synchronization: {error}");
    }
    thread::Builder::new()
        .name("source-access".into())
        .spawn(move || loop {
            if let Err(error) = synchronize(&options, false) {
                eprintln!("source access synchronization: {error}");
            }
            thread::sleep(Duration::from_secs(3));
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn synchronize(options: &Options, force: bool) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    let previous = saved(&options.state_dir).ok().flatten();
    // A first complete read binds retained state to the current enrollment, even
    // if a new node/token happens to have the same fleet policy revision.
    let conditional = previous.as_ref().filter(|_| !force);
    let path = conditional.map_or_else(
        || "/agent/v1/source-access".to_owned(),
        |policy| format!("/agent/v1/source-access?after_revision={}", policy.revision),
    );
    let response =
        client.request_timeout("GET", &path, &options.token, None, Duration::from_secs(5))?;
    if response.status == 404 {
        return Ok(());
    } // Downgrading Console never releases cached bans.
    let desired = match response.status {
        200 => {
            let desired: SourceAccessDesired = serde_json::from_str(&response.body)
                .map_err(|_| "invalid source access response")?;
            persist(&options.state_dir, &desired)?;
            desired
        }
        204 => previous.ok_or("source access snapshot missing after unchanged response")?,
        status => return Err(format!("source access GET returned HTTP {status}")),
    };
    let report = observation(&options.state_dir, &desired);
    let body = serde_json::to_string(&report).map_err(|e| e.to_string())?;
    let response = client.request_timeout(
        "POST",
        "/agent/v1/source-access",
        &options.token,
        Some(&body),
        Duration::from_secs(5),
    )?;
    if response.status != 204 {
        return Err(format!(
            "source access receipt returned HTTP {}",
            response.status
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Receipt {
    revision: i64,
    healthy: bool,
    pid_start_ticks: u64,
    boot_id: String,
}

fn confirmed(dir: &Path, desired: &SourceAccessDesired) -> Result<bool, String> {
    let expected = policy_path(
        &std::fs::canonicalize(dir).map_err(|_| "source access state directory unavailable")?,
    );
    let config =
        std::fs::read_to_string(dir.join("xray.json")).map_err(|_| "等待 Xray 接入配置")?;
    let config: serde_json::Value =
        serde_json::from_str(&config).map_err(|_| "Xray 接入配置无效")?;
    let split = config["inbounds"].as_array().is_some_and(|inbounds| {
        inbounds.iter().any(|inbound| {
            inbound["tag"]
                .as_str()
                .is_some_and(|tag| tag.ends_with(":upload"))
        })
    });
    if split {
        let safe = config["outbounds"].as_array().is_some_and(|outbounds| {
            outbounds.iter().any(|outbound| {
                outbound["tag"] == "out:split-front" && outbound["settings"]["proxyProtocol"] == 2
            })
        });
        if !safe {
            return Err("XHTTP 独立下载入口需要发布新配置以保留真实来源 IP".into());
        }
        if crate::xrayupdate::read_upgrade_runtime(dir)?.is_some() {
            return Err("XHTTP 独立下载入口交接中，等待旧代退出后确认来源策略".into());
        }
    }
    let serving = crate::serving_xray_process_in(dir).ok_or("Xray 尚未运行")?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|_| "boot identity unavailable")?;
    let serving_generation = brocade_deployment::protocol::XrayUpgradeProcess {
        pid: serving.pid,
        pid_start_ticks: serving.start_ticks,
        boot_id: boot.trim().to_owned(),
        sha256: crate::cached_xray_sha256(&PathBuf::from(format!("/proc/{}/exe", serving.pid)))?,
        alive: true,
        accepting: None,
        active_connections: None,
    };
    if !crate::traffic_limit::generation_supports_capability(
        &serving_generation,
        "source-access-v1",
    )? {
        return Ok(false);
    }
    let mut processes = vec![(serving.pid, serving.start_ticks)];
    if let Some(runtime) = crate::xrayupdate::read_upgrade_runtime(dir)? {
        for process in std::iter::once(&runtime.old).chain(runtime.candidate.as_ref()) {
            if crate::process_ref(process.pid)
                .is_some_and(|p| p.start_ticks == process.pid_start_ticks)
            {
                if !crate::traffic_limit::generation_supports_capability(
                    process,
                    "source-access-v1",
                )? {
                    return Ok(false);
                }
                processes.push((process.pid, process.pid_start_ticks));
            }
        }
    }
    processes.sort_unstable();
    processes.dedup();

    for (pid, ticks) in processes {
        let same = || crate::process_ref(pid).is_some_and(|p| p.start_ticks == ticks);
        if !same() {
            return Err("Xray 代际正在切换，等待接入策略确认".into());
        }
        let environment = read_bounded(&PathBuf::from(format!("/proc/{pid}/environ")), 64 * 1024)?
            .ok_or("Xray environment unavailable")?;
        let prefix = format!("{ENV}=");
        if !environment.split(|b| *b == 0).any(|e| {
            e.strip_prefix(prefix.as_bytes())
                .is_some_and(|p| p == expected.as_os_str().as_encoded_bytes())
        }) {
            return Ok(false);
        }
        let Some(bytes) = read_bounded(
            &PathBuf::from(format!("{}.{pid}.json", expected.display())),
            4096,
        )?
        else {
            return Err("等待 Xray 应用接入策略".into());
        };
        let receipt: Receipt =
            serde_json::from_slice(&bytes).map_err(|_| "invalid source access receipt")?;
        if !same() || receipt.pid_start_ticks != ticks || receipt.boot_id != boot.trim() {
            return Err("Xray 接入策略回执身份不一致".into());
        }
        if !receipt.healthy || receipt.revision != desired.revision {
            return Err("等待全部 Xray 代际应用接入策略".into());
        }
    }
    Ok(true)
}
fn observation(dir: &Path, desired: &SourceAccessDesired) -> SourceAccessObservation {
    match confirmed(dir, desired) {
        Ok(true) => SourceAccessObservation {
            revision: desired.revision,
            supported: true,
            error: None,
        },
        Ok(false) => SourceAccessObservation {
            revision: desired.revision,
            supported: false,
            error: Some("Xray 尚不支持来源封禁，请升级 Agent 和 Xray".into()),
        },
        Err(error) => SourceAccessObservation {
            revision: desired.revision,
            supported: true,
            error: Some(error.chars().take(512).collect()),
        },
    }
}

pub(crate) fn verify_executable(dir: &Path, executable: &Path) -> Result<(), String> {
    initialize(dir)?;
    let required = saved(dir)?.is_some_and(|policy| policy.active());
    if required {
        let output = crate::command::run_command_with_timeout(
            executable.to_str().ok_or("invalid Xray path")?,
            &["version"],
            Duration::from_secs(2),
        )?;
        if !crate::xray_version_has_capability(&output, "source-access-v1") {
            return Err("仍有来源封禁策略，不能启动不支持 source-access-v1 的 Xray".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_source_access_never_regresses_and_corruption_recovers_from_authority() {
        let dir =
            std::env::temp_dir().join(format!("brocade-source-access-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        initialize(&dir).unwrap();
        let policy = SourceAccessDesired {
            revision: 7,
            node_ips: vec!["192.0.2.1".into()],
            users: vec![],
        };
        persist(&dir, &policy).unwrap();
        assert!(persist(&dir, &SourceAccessDesired::default()).is_err());
        assert_eq!(saved(&dir).unwrap(), Some(policy.clone()));
        std::fs::write(policy_path(&dir), b"damaged").unwrap();
        assert!(saved(&dir).is_err());
        persist(&dir, &policy).unwrap();
        assert_eq!(saved(&dir).unwrap(), Some(policy));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_source_policy_on_an_existing_node_is_never_replaced_by_empty() {
        let dir = std::env::temp_dir().join(format!(
            "brocade-source-access-missing-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("xray.json"), b"{}").unwrap();
        assert!(initialize(&dir).is_err());
        assert!(!policy_path(&dir).exists());
        let desired = SourceAccessDesired {
            revision: 3,
            node_ips: vec!["192.0.2.1".into()],
            users: vec![],
        };
        persist(&dir, &desired).unwrap();
        assert_eq!(saved(&dir).unwrap(), Some(desired));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
