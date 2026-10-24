//! Availability recovery after the admitted generation dies and the old UDP sockets are gone.
//! Keep the original handoff evidence until a new owner has restored all local desired state.

use super::*;

pub(super) const RECOVERY_FILE: &str = "xray-crash-recovery.json";
pub(super) const FAILURE_FILE: &str = "xray-crash-report.json";
pub(super) const RECOVERED_REPORT_FILE: &str = "xray-crash-recovered-report.json";

#[derive(Serialize, Deserialize)]
struct Recovery {
    runtime: XrayUpgradeRuntime,
    config_sha256: String,
    grants_sha256: Option<String>,
    journal_epochs: [Option<String>; 2],
    failures: u32,
    retry_after_unix_secs: i64,
    /// Explicitly authorizes retirement of the frozen identities after control RPC failure.
    /// Older crash checkpoints only permit restarting a dead candidate.
    #[serde(default)]
    restart_unresponsive: bool,
    /// No candidate was registered before the old generation lost control. Restore the
    /// frozen previous config/executable, never manufacture a successful target upgrade.
    #[serde(default)]
    restart_previous: bool,
}

fn read(state_dir: &Path) -> Result<Option<Recovery>, String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state_dir.join(RECOVERY_FILE))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("打开 Xray 崩溃恢复检查点失败：{error}")),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.len() == 0
        || metadata.len() > MAX_UPGRADE_RUNTIME_BYTES
    {
        return Err("Xray 崩溃恢复检查点的类型、属主、权限、链接数或大小无效".to_owned());
    }
    let mut bytes = Vec::new();
    file.take(MAX_UPGRADE_RUNTIME_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let recovery: Recovery =
        serde_json::from_slice(&bytes).map_err(|_| "Xray 崩溃恢复检查点无效".to_owned())?;
    if bytes.len() as u64 != metadata.len()
        || !valid_upgrade_runtime(&recovery.runtime)
        || (recovery.runtime.candidate.is_none() && !recovery.restart_previous)
        || !valid_sha256(&recovery.config_sha256)
        || recovery
            .grants_sha256
            .as_ref()
            .is_some_and(|digest| !valid_sha256(digest))
        || recovery.journal_epochs[0].is_none()
        || recovery
            .journal_epochs
            .iter()
            .flatten()
            .any(|epoch| epoch.len() != 32 || !epoch.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err("Xray 崩溃恢复检查点身份无效".to_owned());
    }
    Ok(Some(recovery))
}

fn write(state_dir: &Path, recovery: &Recovery) -> Result<(), String> {
    crate::fsutil::atomic_write_private(
        &state_dir.join(RECOVERY_FILE),
        &serde_json::to_vec(recovery).map_err(|error| error.to_string())?,
    )
}

pub(super) fn begin(state_dir: &Path, runtime: &XrayUpgradeRuntime) -> Result<bool, String> {
    begin_recovery(state_dir, runtime, None)
}

pub(super) fn begin_unresponsive(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
    cause: &str,
) -> Result<bool, String> {
    begin_recovery(state_dir, runtime, Some(cause))
}

fn begin_recovery(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
    unresponsive: Option<&str>,
) -> Result<bool, String> {
    let restart_previous = runtime.candidate.is_none() && unresponsive.is_some();
    let candidate = runtime.candidate.as_ref().unwrap_or(&runtime.old);
    if runtime.candidate.is_none() && !restart_previous {
        return Err("崩溃恢复缺少已批准的候选身份".to_owned());
    }
    if unresponsive.is_none() && exact_upgrade_process(candidate).is_some() {
        return Err("候选代仍存活，拒绝进入崩溃重启".to_owned());
    }
    // Check all reusable state before retiring the residual TCP sessions. A configuration
    // handoff must never reopen admission from the obsolete formal authorization checkpoint.
    if !restart_previous {
        candidate_grants_for_recovery(state_dir, runtime)?;
    }
    let config_path = if restart_previous && config_handoff_source(runtime).is_some() {
        state_dir.join(CONFIG_PREVIOUS_FILE)
    } else {
        state_dir.join("xray.json")
    };
    let grants_path = if restart_previous {
        "grants.desired.json"
    } else {
        CONFIG_GRANTS_FILE
    };
    let grants_sha256 = if restart_previous {
        crate::desired_grants_on_disk(state_dir)?;
        state_dir
            .join(grants_path)
            .exists()
            .then(|| file_sha256_hex(&state_dir.join(grants_path)))
            .transpose()?
    } else {
        config_handoff_source(runtime)
            .map(|_| file_sha256_hex(&state_dir.join(grants_path)))
            .transpose()?
    };
    let recovery = Recovery {
        runtime: runtime.clone(),
        config_sha256: file_sha256_hex(&config_path)?,
        grants_sha256,
        journal_epochs: crate::accounting::handoff_journal_epochs(state_dir)?,
        failures: 0,
        retry_after_unix_secs: 0,
        restart_unresponsive: unresponsive.is_some(),
        restart_previous,
    };
    if matches!(runtime.source, XrayUpgradeSource::BinaryRelease) && !restart_previous {
        let mut report = recovered_success_report(state_dir, runtime, false);
        report.outcome = XrayReleaseOutcome::FailedDirty;
        report.error = Some(match unresponsive {
            Some(cause) => format!("交接控制失联，降级为直接重启恢复，现有连接将中断：{cause}"),
            None => "交接候选崩溃，原代无法恢复完整接入；Agent 执行自动重启恢复，残留连接可能中断"
                .to_owned(),
        });
        // A separate bounded slot preserves the crash even if success is recorded offline.
        write_stored_report(state_dir, FAILURE_FILE, report)?;
    }
    write(state_dir, &recovery)?;
    if let Some(cause) = unresponsive {
        eprintln!("xray-update: 交接控制失联，降级为直接重启恢复，现有连接将中断：{cause}");
    }
    resume(state_dir)
}

pub(super) fn resume(state_dir: &Path) -> Result<bool, String> {
    let Some(mut recovery) = read(state_dir)? else {
        return Ok(false);
    };
    if let Some(current) = read_upgrade_runtime(state_dir)? {
        if current.source != recovery.runtime.source
            || current.release_id != recovery.runtime.release_id
            || current.attempt != recovery.runtime.attempt
            || process_identity(&current.old) != process_identity(&recovery.runtime.old)
            || current.candidate.as_ref().map(process_identity)
                != recovery.runtime.candidate.as_ref().map(process_identity)
        {
            return Err("崩溃恢复与当前交接身份不一致".to_owned());
        }
    }
    let now = crate::current_unix_secs()?;
    if now < recovery.retry_after_unix_secs {
        // A backwards wall-clock adjustment must not turn bounded backoff into an hours-long
        // outage. Keep the persisted retry within the same maximum wait after an Agent restart.
        if recovery.retry_after_unix_secs > now.saturating_add(60) {
            recovery.retry_after_unix_secs = now.saturating_add(60);
            write(state_dir, &recovery)?;
        }
        return Ok(true);
    }
    recovery.failures = recovery.failures.saturating_add(1);
    let delay = retry_delay(recovery.failures);
    recovery.retry_after_unix_secs = now.saturating_add(delay);
    // Persist before any signals/spawn: Agent interruption cannot restart a failing binary in a
    // one-second loop. Only one checkpoint and one failure receipt are retained per handoff.
    write(state_dir, &recovery)?;
    if let Err(error) = restore(state_dir, &recovery) {
        recovery.retry_after_unix_secs = crate::current_unix_secs()?.saturating_add(delay);
        write(state_dir, &recovery)
            .map_err(|checkpoint_error| format!("{error}；保存恢复退避失败：{checkpoint_error}"))?;
        return Err(error);
    }
    Ok(true)
}

fn process_identity(process: &XrayUpgradeProcess) -> (i32, u64, &str, &str) {
    (
        process.pid,
        process.pid_start_ticks,
        &process.boot_id,
        &process.sha256,
    )
}

fn retry_delay(failures: u32) -> i64 {
    (5_i64 << failures.saturating_sub(1).min(4)).min(60)
}

fn restore(state_dir: &Path, recovery: &Recovery) -> Result<(), String> {
    let runtime = &recovery.runtime;
    let lost = runtime.candidate.as_ref().unwrap_or(&runtime.old);
    if runtime.candidate.is_none() && !recovery.restart_previous {
        return Err("崩溃恢复缺少候选身份".to_owned());
    }
    let target = managed_target()?;
    validate_managed_target(&target)?;
    if recovery.restart_previous {
        restore_old_handoff_state(state_dir, runtime)?;
    }
    let replacement_executable = if recovery.restart_unresponsive
        && !recovery.restart_previous
        && matches!(runtime.source, XrayUpgradeSource::BinaryRelease)
        && file_sha256_hex(&target).ok().as_deref() != Some(lost.sha256.as_str())
    {
        let process = exact_upgrade_process(lost).ok_or("控制失联恢复缺少候选二进制")?;
        Some(PathBuf::from(format!("/proc/{}/exe", process.pid)))
    } else {
        None
    };
    let executable = replacement_executable.as_deref().unwrap_or(&target);
    verify_xray_path(executable, &lost.sha256)?;
    let config = state_dir.join("xray.json");
    if file_sha256_hex(&config)? != recovery.config_sha256 {
        return Err("崩溃恢复期间目标配置发生变化".to_owned());
    }
    let content = fs::read_to_string(&config).map_err(|error| error.to_string())?;
    let desired_grants = if recovery.restart_previous {
        crate::desired_grants_on_disk(state_dir)?
    } else {
        candidate_grants_for_recovery(state_dir, runtime)?
    };
    let grants_sha256 = if recovery.restart_previous {
        state_dir
            .join("grants.desired.json")
            .exists()
            .then(|| file_sha256_hex(&state_dir.join("grants.desired.json")))
            .transpose()?
    } else {
        config_handoff_source(runtime)
            .map(|_| file_sha256_hex(&state_dir.join(CONFIG_GRANTS_FILE)))
            .transpose()?
    };
    if grants_sha256 != recovery.grants_sha256 {
        return Err("崩溃恢复期间目标授权检查点发生变化".to_owned());
    }
    let api = crate::xray_api_address(&content).unwrap_or(crate::DEFAULT_XRAY_API_ADDRESS);
    // Preflight before killing the residual generation, including on retries after Agent exit.
    run_command_with_timeout(
        &executable.to_string_lossy(),
        &["-test", "-config", &config.to_string_lossy()],
        Duration::from_secs(30),
    )?;
    if let Some(executable) = replacement_executable {
        replace_executable(&executable, &target)?;
        verify_xray_path(&target, &lost.sha256)?;
    }
    let old = exact_upgrade_process(&runtime.old);
    let lost_process = exact_upgrade_process(lost);
    if lost_process.is_some() && !recovery.restart_unresponsive {
        return Err("原候选代重新出现，拒绝执行崩溃重启".to_owned());
    }
    if recovery.restart_unresponsive {
        let processes = crate::serving_xray_processes_in(state_dir);
        if processes
            .iter()
            .any(|process| Some(*process) != old && Some(*process) != lost_process)
        {
            return Err("控制失联恢复发现未登记的 Xray 进程，拒绝终止或覆盖".to_owned());
        }
        // The checkpoint, config, grants and accounting namespace are frozen before any signal.
        // SIGKILL still works for a stopped process; termination itself is bounded to 500 ms.
        if let Some(process) = lost_process {
            crate::terminate_processes(&[process], crate::XRAY_TERM_GRACE, crate::XRAY_KILL_GRACE)?;
        }
    }
    let processes = crate::serving_xray_processes_in(state_dir);
    let replacements = processes
        .iter()
        .copied()
        .filter(|process| Some(*process) != old)
        .collect::<Vec<_>>();
    let process = match replacements.as_slice() {
        [] => {
            if let Some(old) = old {
                crate::terminate_processes(&[old], crate::XRAY_TERM_GRACE, crate::XRAY_KILL_GRACE)?;
            }
            crate::accounting::sync_after_handoff_loss(state_dir, &recovery.journal_epochs)?;
            crate::apply_xray_with_grants(&config, api, desired_grants.as_deref())?;
            crate::serving_xray_process_in(state_dir).ok_or("崩溃恢复后 Xray 进程不存在")?
        }
        [process] if old.is_none() => {
            // Agent may have exited after spawn or admission but before clearing the checkpoint.
            // Adopt only an authenticated bootstrap with the frozen executable and config.
            verify_xray_path(
                &PathBuf::from(format!("/proc/{}/exe", process.pid)),
                &lost.sha256,
            )?;
            let (manifest, status) = observe_handoff_process(*process)?;
            if crate::accounting::handoff_journal_epochs(state_dir)? != recovery.journal_epochs {
                return Err("重启候选未延续冻结的计量命名空间".to_owned());
            }
            wait_for_candidate_api(api, &manifest, *process)?;
            let restored = crate::restore_xray_runtime_with_grants(
                state_dir,
                &content,
                api,
                desired_grants.as_deref(),
            );
            if let Some(error) = restored.error() {
                return Err(format!("恢复重启候选动态状态失败：{error}"));
            }
            if !status.accepting {
                let bootstrap = BootstrapHandoff {
                    manifest_path: manifest
                        .control_path
                        .parent()
                        .ok_or("恢复代缺少目录")?
                        .join("manifest.json"),
                    manifest,
                };
                activate_bootstrap_handoff(&bootstrap, process.pid)?;
            }
            *process
        }
        _ => return Err("崩溃恢复发现未登记的 Xray 进程，拒绝终止或覆盖".to_owned()),
    };
    #[cfg(test)]
    if std::env::var_os("BROCADE_TEST_CRASH_RECOVERY_INTERRUPT_AFTER_LAUNCH").is_some() {
        return Err("test interruption after crash recovery launch".to_owned());
    }
    let (_, status) = observe_handoff_process(process)?;
    if !status.ready || !status.accepting || !status.retirement_supported {
        return Err("崩溃恢复后 Xray 尚未完成接入认证".to_owned());
    }
    crate::wait_for_xray_runtime_health(state_dir, &content)?;
    if matches!(runtime.source, XrayUpgradeSource::BinaryRelease) {
        let report = if recovery.restart_previous {
            XrayReleaseReport {
                release_id: runtime.release_id,
                attempt: runtime.attempt,
                outcome: XrayReleaseOutcome::FailedRecovered,
                performed_update: false,
                strategy: runtime.strategy,
                forced_retirement: false,
                xray_enabled: true,
                installed_sha256: Some(runtime.old.sha256.clone()),
                running_sha256: Some(runtime.old.sha256.clone()),
                error: Some("切流前旧代控制失联，已重启恢复发布前版本；现有连接中断".to_owned()),
            }
        } else {
            recovered_success_report(state_dir, runtime, false)
        };
        write_stored_report(state_dir, RECOVERED_REPORT_FILE, report)?;
    }
    if !recovery.restart_previous {
        commit_config_handoff_state(state_dir, runtime)?;
    }
    cleanup_file(&state_dir.join(UPGRADE_RUNTIME_FILE));
    cleanup_file(&state_dir.join(RECOVERY_FILE));
    cleanup_handoff_backup(state_dir, runtime);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_backoff_is_bounded_and_survives_counter_saturation() {
        assert_eq!(
            (1..=6).map(retry_delay).collect::<Vec<_>>(),
            [5, 10, 20, 40, 60, 60]
        );
        assert_eq!(retry_delay(u32::MAX), 60);
    }

    #[test]
    fn offline_crash_receipt_is_retried_before_the_recovered_success() {
        use std::{io::Write, net::TcpListener};

        let root =
            std::env::temp_dir().join(format!("brocade-crash-report-{}", std::process::id()));
        crate::create_private_dir(&root).unwrap();
        let report = |outcome| XrayReleaseReport {
            release_id: 91,
            attempt: 3,
            outcome,
            performed_update: true,
            strategy: XrayReleaseStrategy::Handoff,
            forced_retirement: false,
            xray_enabled: true,
            installed_sha256: Some("b".repeat(64)),
            running_sha256: Some("b".repeat(64)),
            error: None,
        };
        for (name, outcome) in [
            (FAILURE_FILE, XrayReleaseOutcome::FailedDirty),
            (RECOVERED_REPORT_FILE, XrayReleaseOutcome::Succeeded),
        ] {
            crate::fsutil::atomic_write_private(
                &root.join(name),
                &serde_json::to_vec(&StoredReport {
                    report: report(outcome),
                })
                .unwrap(),
            )
            .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let mut received = Vec::new();
            for status in [500, 200, 200] {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "missing receipt request");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("receipt listener: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut raw = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0, "truncated receipt");
                    raw.extend_from_slice(&buffer[..count]);
                    let Some(end) = raw.windows(4).position(|part| part == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = std::str::from_utf8(&raw[..end])
                        .unwrap()
                        .to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse::<usize>()
                        .unwrap();
                    if raw.len() >= end + 4 + length {
                        let report: XrayReleaseReport =
                            serde_json::from_slice(&raw[end + 4..end + 4 + length]).unwrap();
                        received.push(report.outcome);
                        break;
                    }
                }
                let body = r#"{"accepted":true}"#;
                write!(stream, "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            received
        });
        let options = Options {
            command: "run".to_owned(),
            server: format!("http://{address}"),
            token: "fixture".to_owned(),
            state_dir: root.clone(),
            apply_mode: ApplyMode::StateDir,
            vpngate_stats_window: Duration::from_secs(900),
        };
        assert!(retry_stored_report(&options).is_err());
        assert!(root.join(FAILURE_FILE).exists());
        assert!(root.join(RECOVERED_REPORT_FILE).exists());
        retry_stored_report(&options).unwrap();
        assert_eq!(
            server.join().unwrap(),
            [
                XrayReleaseOutcome::FailedDirty,
                XrayReleaseOutcome::FailedDirty,
                XrayReleaseOutcome::Succeeded
            ]
        );
        assert!(!root.join(FAILURE_FILE).exists());
        assert!(!root.join(RECOVERED_REPORT_FILE).exists());
        fs::remove_dir_all(root).unwrap();
    }
}
