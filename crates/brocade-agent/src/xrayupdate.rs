//! Pull, validate and safely install an explicitly released Xray binary.
//!
//! Downloading runs off the convergence thread because a slow transfer must not delay desired
//! state. The irreversible part is handed back to the main loop, which is the sole owner of Xray
//! restarts and can hold the same usage-meter lock as configuration convergence.

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use brocade_deployment::protocol::{XrayReleaseOffer, XrayReleaseOutcome, XrayReleaseReport};
use serde::{Deserialize, Serialize};

use crate::{
    command::run_command_with_timeout, file_sha256_hex, http::HttpClient, identity, shell_quote,
    ApplyMode, Options, WorkloadRuntime, ARCH_HEADER,
};

const UPDATE_INTERVAL: Duration = Duration::from_secs(10 * 60);
const UPDATE_JITTER_MAX_SECS: u64 = 2 * 60;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(310);
const MAX_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;
const CANDIDATE_FILE: &str = "xray-release.candidate";
const DOWNLOAD_FILE: &str = "xray-release.download";
const PREVIOUS_FILE: &str = "xray-release.previous";
const REPORT_FILE: &str = "xray-release-report.json";

#[derive(Debug, Clone)]
struct PreparedUpdate {
    offer: XrayReleaseOffer,
    candidate: PathBuf,
}

#[derive(Default)]
pub(crate) struct XrayUpdateQueue {
    pending: Mutex<Option<PreparedUpdate>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredReport {
    report: XrayReleaseReport,
}

#[derive(Debug, Deserialize)]
struct ReportAck {
    accepted: bool,
}

pub(crate) fn spawn_xray_updates(options: &Options, queue: &Arc<XrayUpdateQueue>) {
    let options = options.clone();
    let queue = Arc::clone(queue);
    let spawned = thread::Builder::new()
        .name("xray-update".to_owned())
        .spawn(move || loop {
            crate::each_round("xray-update", || {
                if let Err(error) = poll_cycle(&options, &queue) {
                    crate::warn(format!("xray-update: {error}"));
                }
            });
            thread::sleep(update_interval(&options.token));
        });
    if let Err(error) = spawned {
        crate::warn(format!(
            "xray-update: 起不了更新线程（{error}）；Xray 只能靠 install.sh 升级"
        ));
    }
}

fn update_interval(node_token: &str) -> Duration {
    // Enrollment tokens are random per machine. Folding their bytes produces a stable offset
    // without another RNG or ever logging the credential, so a wave does not wake every Agent on
    // the same ten-minute boundary.
    let jitter = node_token.as_bytes().iter().fold(0_u64, |state, byte| {
        state.wrapping_mul(131).wrapping_add(u64::from(*byte))
    }) % (UPDATE_JITTER_MAX_SECS + 1);
    UPDATE_INTERVAL + Duration::from_secs(jitter)
}

pub(crate) fn apply_pending(
    options: &Options,
    meter: &Arc<Mutex<()>>,
    queue: &Arc<XrayUpdateQueue>,
) -> Result<(), String> {
    let Some(prepared) = queue
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return Ok(());
    };

    let current = match fetch_offer(options) {
        Ok(current) => current,
        Err(error) => {
            put_back(queue, prepared);
            return Err(format!("替换前无法复核发布：{error}"));
        }
    };
    let Some(current) = current else {
        cleanup_file(&prepared.candidate);
        return Ok(());
    };
    if current.release_id != prepared.offer.release_id
        || current.attempt != prepared.offer.attempt
        || current.sha256 != prepared.offer.sha256
    {
        cleanup_file(&prepared.candidate);
        return Ok(());
    }
    let staged_sha = match file_sha256_hex(&prepared.candidate) {
        Ok(sha) => sha,
        Err(error) => {
            cleanup_file(&prepared.candidate);
            return finish_report(
                options,
                failure_report(
                    options,
                    &current,
                    pre_update_failure_outcome(options, &current),
                    format!("安装前读不到暂存的 Xray 摘要，没有替换：{error}"),
                ),
            );
        }
    };
    if staged_sha != current.sha256 {
        cleanup_file(&prepared.candidate);
        return finish_report(
            options,
            failure_report(
                options,
                &current,
                pre_update_failure_outcome(options, &current),
                "暂存的 Xray 在安装前摘要发生变化，没有替换".to_owned(),
            ),
        );
    }

    let report = perform_update(options, meter, &current, &prepared.candidate);
    cleanup_file(&prepared.candidate);
    if report.outcome != XrayReleaseOutcome::FailedDirty {
        cleanup_file(&options.state_dir.join(PREVIOUS_FILE));
    }
    let _ = crate::publish_runtime_now(options);
    finish_report(options, report)
}

fn put_back(queue: &Arc<XrayUpdateQueue>, prepared: PreparedUpdate) {
    let mut pending = queue
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.is_none() {
        *pending = Some(prepared);
    }
}

fn poll_cycle(options: &Options, queue: &Arc<XrayUpdateQueue>) -> Result<(), String> {
    retry_stored_report(options)?;
    if queue
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
    {
        return Ok(());
    }
    let Some(offer) = fetch_offer(options)? else {
        return Ok(());
    };
    if let Err(error) = validate_offer(&offer) {
        return finish_report(
            options,
            failure_report(options, &offer, XrayReleaseOutcome::Unsupported, error),
        );
    }

    if options.apply_mode != ApplyMode::Linux {
        return finish_report(
            options,
            failure_report(
                options,
                &offer,
                XrayReleaseOutcome::Unsupported,
                "Agent 运行在 state-dir 模式，不能替换系统 Xray".to_owned(),
            ),
        );
    }

    let enabled = xray_enabled(&options.state_dir);
    let installed = crate::installed_xray_sha256();
    let running = crate::running_xray_sha256();
    if installed.as_deref() == Some(offer.sha256.as_str())
        && (!enabled || running.as_deref() == Some(offer.sha256.as_str()))
    {
        return finish_report(
            options,
            success_report(&offer, false, enabled, installed, running),
        );
    }
    if installed.as_deref() != Some(offer.previous_sha256.as_str())
        && running.as_deref() != Some(offer.previous_sha256.as_str())
        && !valid_previous_backup(options, &offer)
    {
        return finish_report(
            options,
            report_with_state(
                &offer,
                XrayReleaseOutcome::FailedDirty,
                false,
                enabled,
                installed,
                running,
                Some("本机 Xray 与发布创建时冻结的摘要都不一致，没有替换".to_owned()),
            ),
        );
    }

    let download = options.state_dir.join(DOWNLOAD_FILE);
    let candidate = options.state_dir.join(CANDIDATE_FILE);
    cleanup_file(&download);
    cleanup_file(&candidate);
    if let Err(error) = fetch(&offer.url, &download) {
        return record_preparation_failure(options, &offer, &download, &candidate, error);
    }
    if let Err(error) = validate_download_size(&download) {
        return record_preparation_failure(options, &offer, &download, &candidate, error);
    }
    let got = match file_sha256_hex(&download) {
        Ok(got) => got,
        Err(error) => {
            return record_preparation_failure(
                options,
                &offer,
                &download,
                &candidate,
                format!("计算下载的 Xray 摘要失败：{error}"),
            )
        }
    };
    if got != offer.sha256 {
        cleanup_file(&download);
        return finish_report(
            options,
            failure_report(
                options,
                &offer,
                pre_update_failure_outcome(options, &offer),
                format!(
                    "下载的 Xray 摘要不符：需要 {}，实际 {got}，没有替换",
                    offer.sha256
                ),
            ),
        );
    }
    if let Err(error) = fs::set_permissions(&download, fs::Permissions::from_mode(0o700)) {
        return record_preparation_failure(
            options,
            &offer,
            &download,
            &candidate,
            format!("给候选 Xray 加执行位失败：{error}"),
        );
    }
    if let Err(error) = preflight(&download, &offer, options) {
        cleanup_file(&download);
        return finish_report(
            options,
            failure_report(
                options,
                &offer,
                pre_update_failure_outcome(options, &offer),
                format!("候选 Xray 预检失败，没有替换：{error}"),
            ),
        );
    }
    if let Err(error) = fs::rename(&download, &candidate) {
        return record_preparation_failure(
            options,
            &offer,
            &download,
            &candidate,
            format!("暂存候选 Xray 失败：{error}"),
        );
    }

    let mut pending = queue
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.is_none() {
        println!(
            "xray-update: 已准备发布 #{} 第 {} 次尝试，等待收敛轮次边界",
            offer.release_id, offer.attempt
        );
        *pending = Some(PreparedUpdate { offer, candidate });
    } else {
        cleanup_file(&candidate);
    }
    Ok(())
}

fn fetch_offer(options: &Options) -> Result<Option<XrayReleaseOffer>, String> {
    let client = HttpClient::new(&options.server)?;
    let response = client.request_with_headers(
        "GET",
        "/agent/v1/xray-release",
        &options.token,
        None,
        &[(ARCH_HEADER, identity::self_arch().to_owned())],
    )?;
    if response.status == 204 {
        return Ok(None);
    }
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "问不到 Xray 发布：HTTP {} {}",
            response.status, response.body
        ));
    }
    serde_json::from_str(&response.body)
        .map(Some)
        .map_err(|error| format!("Xray 发布响应读不懂：{error}"))
}

fn validate_offer(offer: &XrayReleaseOffer) -> Result<(), String> {
    if offer.release_id <= 0 || offer.attempt == 0 {
        return Err("Xray 发布身份超出范围".to_owned());
    }
    for (name, value) in [
        ("sha256", offer.sha256.as_str()),
        ("previous_sha256", offer.previous_sha256.as_str()),
    ] {
        if value.len() != 64
            || !value
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        {
            return Err(format!("Xray 发布的 {name} 不成形"));
        }
    }
    if !(offer.url.starts_with("https://") || offer.url.starts_with("http://")) {
        return Err("Xray 发布下载地址必须使用 HTTP 或 HTTPS".to_owned());
    }
    if offer.version.trim().is_empty() || offer.version.chars().count() > 128 {
        return Err("Xray 发布版本缺失或过长".to_owned());
    }
    Ok(())
}

fn fetch(url: &str, destination: &Path) -> Result<(), String> {
    crate::run_shell_with_timeout(
        &format!(
            "curl -fsSL --proto '=http,https' --proto-redir '=http,https' --connect-timeout 15 --max-time 300 --max-filesize {MAX_DOWNLOAD_BYTES} {} -o {}",
            shell_quote(url),
            shell_quote(&destination.to_string_lossy())
        ),
        DOWNLOAD_TIMEOUT,
    )
    .map(|_| ())
    .map_err(|error| format!("下载 Xray 失败：{error}"))
}

fn validate_download_size(path: &Path) -> Result<(), String> {
    let bytes = fs::metadata(path)
        .map_err(|error| format!("读取下载的 Xray 大小失败：{error}"))?
        .len();
    if bytes == 0 || bytes > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "下载的 Xray 大小 {bytes} 字节不在 1..={MAX_DOWNLOAD_BYTES} 的安全范围内"
        ));
    }
    Ok(())
}

fn record_preparation_failure(
    options: &Options,
    offer: &XrayReleaseOffer,
    download: &Path,
    candidate: &Path,
    error: String,
) -> Result<(), String> {
    cleanup_file(download);
    cleanup_file(candidate);
    finish_report(
        options,
        failure_report(
            options,
            offer,
            pre_update_failure_outcome(options, offer),
            format!("候选 Xray 准备失败，没有替换：{error}"),
        ),
    )
}

fn preflight(candidate: &Path, offer: &XrayReleaseOffer, options: &Options) -> Result<(), String> {
    let program = candidate.to_string_lossy().into_owned();
    let version = run_command_with_timeout(&program, &["version"], Duration::from_secs(10))?;
    let expected = offer.version.trim().trim_start_matches('v');
    if !reports_expected_version(&version, expected) {
        return Err(format!(
            "候选 Xray 没报告预期版本 {}：{}",
            offer.version,
            version.lines().next().unwrap_or_default()
        ));
    }
    let config = options.state_dir.join("xray.json");
    if xray_enabled(&options.state_dir) {
        let config = config.to_string_lossy().into_owned();
        run_command_with_timeout(
            &program,
            &["-test", "-config", &config],
            Duration::from_secs(30),
        )?;
    }
    Ok(())
}

fn reports_expected_version(output: &str, expected: &str) -> bool {
    let mut fields = output.lines().next().unwrap_or_default().split_whitespace();
    fields.next() == Some("Xray")
        && fields
            .next()
            .is_some_and(|version| version.trim_start_matches('v') == expected)
}

fn perform_update(
    options: &Options,
    meter: &Arc<Mutex<()>>,
    offer: &XrayReleaseOffer,
    candidate: &Path,
) -> XrayReleaseReport {
    let target = match managed_target() {
        Ok(target) => target,
        Err(error) => {
            return failure_report(options, offer, XrayReleaseOutcome::Unsupported, error)
        }
    };
    if let Err(error) = validate_managed_target(&target) {
        return failure_report(options, offer, XrayReleaseOutcome::Unsupported, error);
    }
    perform_update_at_target(options, meter, offer, candidate, &target)
}

fn perform_update_at_target(
    options: &Options,
    meter: &Arc<Mutex<()>>,
    offer: &XrayReleaseOffer,
    candidate: &Path,
    target: &Path,
) -> XrayReleaseReport {
    let enabled = xray_enabled(&options.state_dir);
    let installed = file_sha256_hex(target).ok();
    let running = crate::running_xray_sha256();
    if installed.as_deref() == Some(offer.sha256.as_str())
        && (!enabled || running.as_deref() == Some(offer.sha256.as_str()))
    {
        return success_report(offer, false, enabled, installed, running);
    }

    // This candidate may have waited while a configuration deployment converged. Test the current
    // file again at the boundary so an incompatible binary never reaches the rename merely because
    // it was compatible with the configuration from ten minutes ago.
    if let Err(error) = preflight(candidate, offer, options) {
        return failure_report(
            options,
            offer,
            pre_update_failure_outcome(options, offer),
            format!("候选 Xray 在替换前复检失败，没有替换：{error}"),
        );
    }
    let backup = options.state_dir.join(PREVIOUS_FILE);
    if let Err(error) = prepare_previous(target, &backup, offer) {
        let outcome = classify_failure(
            enabled,
            installed.as_deref(),
            running.as_deref(),
            &offer.previous_sha256,
        );
        return report_with_state(
            offer,
            outcome,
            false,
            enabled,
            installed,
            running,
            Some(error),
        );
    }

    if !enabled {
        let attempted = replace_executable(candidate, target)
            .and_then(|()| verify_xray_path(target, offer.sha256.as_str()));
        return match attempted {
            Ok(()) => success_report(
                offer,
                true,
                false,
                file_sha256_hex(target).ok(),
                crate::running_xray_sha256(),
            ),
            Err(update_error) => {
                let recovered = replace_executable(&backup, target)
                    .and_then(|()| verify_xray_path(target, offer.previous_sha256.as_str()));
                match recovered {
                    Ok(()) => failure_report(
                        options,
                        offer,
                        XrayReleaseOutcome::FailedRecovered,
                        format!("新 Xray 未能完整落盘，已恢复旧版本：{update_error}"),
                    ),
                    Err(recovery_error) => failure_report(
                        options,
                        offer,
                        XrayReleaseOutcome::FailedDirty,
                        format!(
                            "新 Xray 更新失败：{update_error}；恢复旧版本也失败：{recovery_error}"
                        ),
                    ),
                }
            }
        };
    }

    let _meter = meter
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(error) = crate::collect_usage_report(options) {
        crate::warn(format!("xray-update: 重启前用量采集失败：{error}"));
    }
    let attempted = replace_executable(candidate, target)
        .and_then(|()| start_and_restore_runtime(options, offer.sha256.as_str()));
    if let Err(update_error) = attempted {
        let recovered = replace_executable(&backup, target)
            .and_then(|()| start_and_restore_runtime(options, offer.previous_sha256.as_str()));
        return match recovered {
            Ok(()) => report_with_state(
                offer,
                XrayReleaseOutcome::FailedRecovered,
                false,
                true,
                crate::installed_xray_sha256(),
                crate::running_xray_sha256(),
                Some(format!(
                    "新 Xray 未通过启动检查，已恢复旧版本：{update_error}"
                )),
            ),
            Err(recovery_error) => report_with_state(
                offer,
                XrayReleaseOutcome::FailedDirty,
                false,
                true,
                crate::installed_xray_sha256(),
                crate::running_xray_sha256(),
                Some(format!(
                    "新 Xray 更新失败：{update_error}；恢复旧版本也失败：{recovery_error}"
                )),
            ),
        };
    }
    success_report(
        offer,
        true,
        true,
        crate::installed_xray_sha256(),
        crate::running_xray_sha256(),
    )
}

fn start_and_restore_runtime(options: &Options, expected_sha256: &str) -> Result<(), String> {
    let config = options.state_dir.join("xray.json");
    let content =
        fs::read_to_string(&config).map_err(|error| format!("读取当前 Xray 配置失败：{error}"))?;
    let api_port = crate::xray_api_port(&content).unwrap_or(10085);
    crate::apply_xray(&config, api_port)?;
    match crate::xray_runtime(&content) {
        WorkloadRuntime::Healthy => {}
        WorkloadRuntime::Broken(error) | WorkloadRuntime::Unknown(error) => {
            return Err(format!("Xray 启动后健康检查失败：{error}"));
        }
    }
    if let Some(grants) = crate::desired_grants_on_disk(&options.state_dir)? {
        crate::sync_grants(&content, api_port, &grants)?;
    }
    verify_installed_xray(expected_sha256)?;
    let running =
        crate::running_xray_sha256().ok_or_else(|| "Xray 启动后读不到运行中摘要".to_owned())?;
    if running != expected_sha256 {
        return Err(format!(
            "Xray 启动后运行的是 {running}，不是预期的 {expected_sha256}"
        ));
    }
    Ok(())
}

fn verify_installed_xray(expected_sha256: &str) -> Result<(), String> {
    let installed =
        crate::installed_xray_sha256().ok_or_else(|| "读不到受管路径上的 Xray 摘要".to_owned())?;
    if installed != expected_sha256 {
        return Err(format!(
            "受管路径上的 Xray 是 {installed}，不是预期的 {expected_sha256}"
        ));
    }
    Ok(())
}

fn verify_xray_path(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let installed = file_sha256_hex(path)?;
    if installed != expected_sha256 {
        return Err(format!(
            "受管路径上的 Xray 是 {installed}，不是预期的 {expected_sha256}"
        ));
    }
    Ok(())
}

fn managed_target() -> Result<PathBuf, String> {
    let target = crate::which_xray()?;
    if !target.is_absolute() {
        return Err(format!(
            "Xray 发布需要绝对受管路径，当前解析为 {}",
            target.display()
        ));
    }
    Ok(target)
}

fn validate_managed_target(target: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(target)
        .map_err(|error| format!("读取受管 Xray {} 失败：{error}", target.display()))?;
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "受管 Xray 必须是 Agent 账号所有、组和其他用户不可写的普通文件，且不能是符号链接：{}",
            target.display()
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| format!("受管 Xray 没有上级目录：{}", target.display()))?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("读取 Xray 目录 {} 失败：{error}", parent.display()))?;
    if !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.uid() != effective_uid
        || parent_metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "Xray 所在目录必须由 Agent 账号所有且不能被组或其他用户写入：{}",
            parent.display()
        ));
    }
    Ok(())
}

fn prepare_previous(target: &Path, backup: &Path, offer: &XrayReleaseOffer) -> Result<(), String> {
    if file_sha256_hex(backup).ok().as_deref() == Some(offer.previous_sha256.as_str()) {
        return Ok(());
    }
    cleanup_file(backup);
    let installed = file_sha256_hex(target).ok();
    let source = if installed.as_deref() == Some(offer.previous_sha256.as_str()) {
        target.to_path_buf()
    } else if crate::running_xray_sha256().as_deref() == Some(offer.previous_sha256.as_str()) {
        crate::serving_xray_executable_path()
            .ok_or_else(|| "运行中的旧 Xray 在备份前消失了".to_owned())?
    } else {
        return Err("没有与发布前摘要一致的旧 Xray 可供安全恢复".to_owned());
    };
    copy_atomic(&source, backup, 0o700)?;
    let copied = file_sha256_hex(backup)?;
    if copied != offer.previous_sha256 {
        cleanup_file(backup);
        return Err(format!(
            "旧 Xray 备份摘要不符：需要 {}，实际 {copied}",
            offer.previous_sha256
        ));
    }
    Ok(())
}

fn valid_previous_backup(options: &Options, offer: &XrayReleaseOffer) -> bool {
    file_sha256_hex(&options.state_dir.join(PREVIOUS_FILE))
        .ok()
        .as_deref()
        == Some(offer.previous_sha256.as_str())
}

fn replace_executable(source: &Path, destination: &Path) -> Result<(), String> {
    copy_atomic(source, destination, 0o755)
}

fn copy_atomic(source: &Path, destination: &Path, mode: u32) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| format!("{} 没有上级目录", destination.display()))?;
    let name = destination
        .file_name()
        .ok_or_else(|| format!("{} 没有文件名", destination.display()))?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.brocade-new-{}", std::process::id()));
    cleanup_file(&temporary);
    let result = (|| {
        let mut input = File::open(source)
            .map_err(|error| format!("打开 {} 失败：{error}", source.display()))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|error| format!("创建 {} 失败：{error}", temporary.display()))?;
        io::copy(&mut input, &mut output)
            .map_err(|error| format!("写入 {} 失败：{error}", temporary.display()))?;
        output
            .set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|error| format!("chmod {} 失败：{error}", temporary.display()))?;
        output
            .sync_all()
            .map_err(|error| format!("fsync {} 失败：{error}", temporary.display()))?;
        drop(output);
        fs::rename(&temporary, destination).map_err(|error| {
            format!(
                "原子替换 {} 为 {} 失败：{error}",
                temporary.display(),
                destination.display()
            )
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("fsync 目录 {} 失败：{error}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        cleanup_file(&temporary);
    }
    result
}

fn xray_enabled(state_dir: &Path) -> bool {
    state_dir.join("xray.json").exists() && !state_dir.join("xray.disabled").exists()
}

fn success_report(
    offer: &XrayReleaseOffer,
    performed_update: bool,
    enabled: bool,
    installed: Option<String>,
    running: Option<String>,
) -> XrayReleaseReport {
    report_with_state(
        offer,
        XrayReleaseOutcome::Succeeded,
        performed_update,
        enabled,
        installed,
        running,
        None,
    )
}

fn failure_report(
    options: &Options,
    offer: &XrayReleaseOffer,
    outcome: XrayReleaseOutcome,
    error: String,
) -> XrayReleaseReport {
    report_with_state(
        offer,
        outcome,
        false,
        xray_enabled(&options.state_dir),
        crate::installed_xray_sha256(),
        crate::running_xray_sha256(),
        Some(error),
    )
}

fn pre_update_failure_outcome(options: &Options, offer: &XrayReleaseOffer) -> XrayReleaseOutcome {
    let enabled = xray_enabled(&options.state_dir);
    let installed = crate::installed_xray_sha256();
    let running = crate::running_xray_sha256();
    classify_failure(
        enabled,
        installed.as_deref(),
        running.as_deref(),
        &offer.previous_sha256,
    )
}

/// “已恢复” describes the durable managed path, not merely an old process whose inode is still
/// alive after that path changed. The latter is exactly the dirty state operators must see: the
/// next restart would otherwise silently start bytes different from the frozen baseline.
fn classify_failure(
    enabled: bool,
    installed: Option<&str>,
    running: Option<&str>,
    previous_sha256: &str,
) -> XrayReleaseOutcome {
    if installed == Some(previous_sha256) && (!enabled || running == Some(previous_sha256)) {
        XrayReleaseOutcome::FailedRecovered
    } else {
        XrayReleaseOutcome::FailedDirty
    }
}

fn report_with_state(
    offer: &XrayReleaseOffer,
    outcome: XrayReleaseOutcome,
    performed_update: bool,
    enabled: bool,
    installed: Option<String>,
    running: Option<String>,
    error: Option<String>,
) -> XrayReleaseReport {
    XrayReleaseReport {
        release_id: offer.release_id,
        attempt: offer.attempt,
        outcome,
        performed_update,
        xray_enabled: enabled,
        installed_sha256: installed,
        running_sha256: running,
        error: error.map(|error| error.chars().take(4_000).collect()),
    }
}

fn finish_report(options: &Options, report: XrayReleaseReport) -> Result<(), String> {
    let stored = StoredReport {
        report: report.clone(),
    };
    let encoded = serde_json::to_vec(&stored).map_err(|error| error.to_string())?;
    crate::fsutil::atomic_write_private(&options.state_dir.join(REPORT_FILE), &encoded)?;
    let _accepted = send_report(options, &report)?;
    cleanup_file(&options.state_dir.join(REPORT_FILE));
    cleanup_after_report(options, report.outcome);
    Ok(())
}

fn retry_stored_report(options: &Options) -> Result<(), String> {
    let path = options.state_dir.join(REPORT_FILE);
    let stored = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<StoredReport>(&bytes)
            .map_err(|error| format!("读不懂待补发的 Xray 更新结果：{error}"))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取待补发 Xray 更新结果失败：{error}")),
    };
    let _accepted = send_report(options, &stored.report)?;
    cleanup_file(&path);
    cleanup_after_report(options, stored.report.outcome);
    Ok(())
}

fn cleanup_after_report(options: &Options, outcome: XrayReleaseOutcome) {
    cleanup_file(&options.state_dir.join(DOWNLOAD_FILE));
    cleanup_file(&options.state_dir.join(CANDIDATE_FILE));
    // A dirty failure deliberately retains the frozen old binary for manual recovery. Every
    // other terminal result says either that the new process is healthy or that the managed path
    // contains the old bytes again, so keeping another executable indefinitely buys nothing.
    if outcome != XrayReleaseOutcome::FailedDirty {
        cleanup_file(&options.state_dir.join(PREVIOUS_FILE));
    }
}

fn send_report(options: &Options, report: &XrayReleaseReport) -> Result<bool, String> {
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/xray-release/report",
        &options.token,
        Some(&body),
    )?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "Xray 更新结果上报失败：HTTP {} {}",
            response.status, response.body
        ));
    }
    serde_json::from_str::<ReportAck>(&response.body)
        .map(|ack| ack.accepted)
        .map_err(|error| format!("Xray 更新结果响应读不懂：{error}"))
}

fn cleanup_file(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("brocade-xray-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn atomic_executable_replacement_keeps_complete_bytes_and_mode() {
        let dir = temp_dir("replace");
        let source = dir.join("candidate");
        let target = dir.join("xray");
        fs::write(&source, b"new-complete-binary").unwrap();
        fs::write(&target, b"old").unwrap();

        replace_executable(&source, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new-complete-binary");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains("brocade-new"))
                .count(),
            0
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rollout_interval_has_a_stable_bounded_per_node_jitter() {
        let first = update_interval("broc_node_first");
        assert_eq!(first, update_interval("broc_node_first"));
        assert!(first >= UPDATE_INTERVAL);
        assert!(first <= UPDATE_INTERVAL + Duration::from_secs(UPDATE_JITTER_MAX_SECS));
    }

    #[test]
    fn downloaded_candidate_must_be_nonempty_and_bounded() {
        let dir = temp_dir("download-size");
        let path = dir.join("candidate");
        fs::write(&path, []).unwrap();
        assert!(validate_download_size(&path).is_err());
        File::create(&path)
            .unwrap()
            .set_len(MAX_DOWNLOAD_BYTES + 1)
            .unwrap();
        assert!(validate_download_size(&path).is_err());
        fs::write(&path, b"x").unwrap();
        assert!(validate_download_size(&path).is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn disabled_rollout_replaces_the_managed_binary_end_to_end() {
        let dir = temp_dir("disabled-rollout");
        let target = dir.join("xray");
        let candidate = dir.join("candidate");
        let old = b"#!/bin/sh\necho 'Xray 26.4.25 old'\n";
        let next = b"#!/bin/sh\necho 'Xray 26.9.1 candidate'\n";
        fs::write(&target, old).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&candidate, next).unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700)).unwrap();
        let previous_sha256 = file_sha256_hex(&target).unwrap();
        let sha256 = file_sha256_hex(&candidate).unwrap();
        let offer = XrayReleaseOffer {
            release_id: 7,
            attempt: 1,
            version: "26.9.1".to_owned(),
            url: "https://console.invalid/brocade-xray/x86_64".to_owned(),
            sha256: sha256.clone(),
            previous_sha256,
        };
        let options = Options {
            command: "run".to_owned(),
            server: "https://console.invalid".to_owned(),
            token: "broc_node_test".to_owned(),
            state_dir: dir.clone(),
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: std::time::Duration::from_secs(900),
        };

        let report = perform_update_at_target(
            &options,
            &Arc::new(Mutex::new(())),
            &offer,
            &candidate,
            &target,
        );

        assert_eq!(report.outcome, XrayReleaseOutcome::Succeeded);
        assert!(report.performed_update);
        assert!(!report.xray_enabled);
        assert_eq!(report.installed_sha256.as_deref(), Some(sha256.as_str()));
        assert_eq!(fs::read(&target).unwrap(), next);
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let no_op = perform_update_at_target(
            &options,
            &Arc::new(Mutex::new(())),
            &offer,
            &candidate,
            &target,
        );
        assert_eq!(no_op.outcome, XrayReleaseOutcome::Succeeded);
        assert!(!no_op.performed_update);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn offer_requires_lowercase_full_digests() {
        let mut offer = XrayReleaseOffer {
            release_id: 1,
            attempt: 1,
            version: "v1".to_owned(),
            url: "https://console/xray".to_owned(),
            sha256: "a".repeat(64),
            previous_sha256: "b".repeat(64),
        };
        assert!(validate_offer(&offer).is_ok());
        offer.sha256 = "A".repeat(64);
        assert!(validate_offer(&offer).is_err());
        offer.sha256 = "a".repeat(64);
        offer.url = "file:///tmp/xray".to_owned();
        assert!(validate_offer(&offer).is_err());
    }

    #[test]
    fn recovered_means_the_managed_path_is_back_on_the_frozen_digest() {
        let previous = "b".repeat(64);
        assert_eq!(
            classify_failure(true, Some(&previous), Some(&previous), &previous),
            XrayReleaseOutcome::FailedRecovered
        );
        assert_eq!(
            classify_failure(true, Some(&"c".repeat(64)), Some(&previous), &previous),
            XrayReleaseOutcome::FailedDirty
        );
        assert_eq!(
            classify_failure(true, Some(&previous), None, &previous),
            XrayReleaseOutcome::FailedDirty
        );
        assert_eq!(
            classify_failure(false, Some(&previous), None, &previous),
            XrayReleaseOutcome::FailedRecovered
        );
        assert_eq!(
            classify_failure(false, None, None, &previous),
            XrayReleaseOutcome::FailedDirty
        );
    }

    #[test]
    fn version_preflight_compares_the_exact_xray_version_field() {
        assert!(reports_expected_version(
            "Xray 26.9.1 (Xray, Penetrates Everything.) baseline (go1.26 linux/amd64)",
            "26.9.1"
        ));
        assert!(!reports_expected_version(
            "Xray 126.9.10 anything",
            "26.9.1"
        ));
        assert!(!reports_expected_version("not-xray 26.9.1", "26.9.1"));
    }

    #[test]
    fn managed_target_rejects_a_group_writable_executable() {
        let dir = temp_dir("unsafe-mode");
        let target = dir.join("xray");
        fs::write(&target, b"binary").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o775)).unwrap();

        assert!(validate_managed_target(&target).is_err());

        let _ = fs::remove_dir_all(dir);
    }
}
