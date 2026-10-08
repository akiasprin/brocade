//! Replacing the binary this process is running.
//!
//! # Why the agent has to do this at all
//!
//! Every other component can be reached: xray and phantun are fetched by the agent, configuration
//! arrives in the desired state. The agent is the one link with none of that — it sits on other
//! people's machines, cannot be pushed to, and may not admit SSH either. Before this, upgrading it
//! meant re-running `install.sh`, which means logging in, which is the one thing the pull model
//! exists to stop requiring.
//!
//! # The order of operations, and why each step is where it is
//!
//! 1. Ask. 204 means nothing to do, which is the answer nearly every round.
//! 2. Compare against the sha256 of the running binary (`identity.rs`). Equal means this machine
//!    already runs these bytes. A tracked release still needs a durable result; digest equality
//!    alone must not be mistaken for evidence that this process performed the update.
//! 3. Download to the state directory, which is 0700.
//! 4. Verify the sha256 before anything is executed or installed.
//! 5. **Run the candidate once** before it replaces anything. This is what stops a binary that
//!    cannot execute on this machine — wrong architecture, truncated download, a libc assumption
//!    that does not hold here — from becoming the installed one. `install.sh` gates on the same
//!    signal for the same reason.
//! 6. Recheck the assignment and persist its pending receipt, then stage next to the real binary
//!    and `rename` over it. Not `install`(1) and not writing in
//!    place: opening a running executable for writing returns `ETXTBSY`. `rename` is atomic, and
//!    the running process keeps its own inode until it exits.
//! 7. Ask the main loop to exit. The installed service supervisor starts the new one five seconds
//!    later (`Restart=always` on systemd, `supervise-daemon` on OpenRC).
//! 8. The new process reports installed/running digests from the pending receipt. Console also
//!    requires fresh polling and runtime reports from these bytes. Retry the durable receipt
//!    until acknowledged; a legacy updater without receipts is explicitly observation-confirmed.
//!
//! # This depends on something restarting the process
//!
//! Step 7 hands the last move to the supervisor. `install.sh` writes either a systemd unit or an
//! OpenRC service; both respawn the process. Installed with `--service-mode foreground` — which is
//! what the preview containers use (`brocade-preview`'s `provision.rs`) — nothing restarts it, so a
//! self-update stops the agent and leaves it stopped.
//!
//! Not guarded against here, because the agent cannot find out: the mode is a decision made by the
//! installer, and nothing about it reaches the process (the env file carries only server, token,
//! state dir, and apply mode). Guessing from PID 1 or from the absence of `systemctl` would be a
//! guess, and a wrong one either disables self-update on a real node or fails to disable it in a
//! container. What keeps this from biting is that reaching step 7 requires somebody to have
//! released deliberately, and a preview control plane has released nothing.
//!
//! # What happens when the new agent is bad
//!
//! Nothing rolls back, deliberately. An earlier design kept `agent.prev` and a stale-marker script
//! on `ExecStartPre`, and it was worse than what it prevented: any momentary root on the machine
//! could leave a hostile binary in `agent.prev` plus an expired marker, and the recovery mechanism
//! would faithfully reinstall it — surviving a reinstall of the agent. A repair path that
//! resurrects attacker-chosen bytes is not a repair path.
//!
//! What it costs to drop it is smaller than it sounds. `KillMode=process` keeps xray alive across
//! the agent's exit, and wg0 is a kernel interface — so an agent stuck in a restart loop means the
//! machine stops accepting new configuration and stops reporting usage. It does not mean the
//! machine stops carrying traffic. Nobody's connection drops. The repair is `install.sh` over SSH,
//! and the defences against ever needing it are step 5 above and staged rollout: release to one
//! node, look at it, then release to the rest.
//!
//! # Why the sha256 is not a defence against an attacker
//!
//! The bytes and the sha both come from the control plane over the same connection, so whoever can
//! change one can change the other. It catches a truncated download and a misconfigured URL. What
//! actually carries the weight is https to a publicly-signed certificate (the agent's trust
//! anchors are compiled in, so no CA added to the node's own store can weaken it) and the fact
//! that a person stages the rollout.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use brocade_deployment::protocol::{
    AgentReleaseOffer, AgentReleaseOutcome, AgentReleaseReport, BinarySource,
};
use serde::{Deserialize, Serialize};

use crate::{
    command::{capture_command_with_timeout, run_shell_with_timeout},
    file_sha256_hex,
    http::HttpClient,
    identity,
    options::Options,
    shell_quote, warn, ARCH_HEADER,
};

/// Where the candidate lands while it is being checked. Inside the state directory, which the
/// agent keeps at 0700 — the bytes are public, but a half-written file that is about to be run as
/// root should not be world-readable while it is being decided upon.
const DOWNLOAD_FILE: &str = "agent.download";

/// The name staged beside the real binary for the final rename. Leading dot and a fixed name: it
/// exists for a moment, and a leftover from a crashed round must be overwritten rather than
/// accumulate one file per attempt.
const STAGING_FILE: &str = ".brocade-agent.new";

/// One round. `Ok(false)` means nothing to do.
///
/// Returning rather than exiting here: the process must not disappear in the middle of a
/// convergence, so the decision to leave belongs to the main loop.
const PENDING_FILE: &str = "agent-release-pending.json";
const MAX_PENDING_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingUpdate {
    offer: AgentReleaseOffer,
    /// Frozen path verified before replacement; /proc/self/exe may later name a deleted inode.
    target: PathBuf,
    /// None means replacement was prepared but no new process has confirmed it yet.
    report: Option<AgentReleaseReport>,
}

pub(crate) fn selfupdate_cycle(options: &Options) -> Result<bool, String> {
    let Some(running) = identity::self_sha256() else {
        return Ok(false);
    };
    if let Some(mut pending) = read_pending(&options.state_dir.join(PENDING_FILE))? {
        if pending.report.is_none() {
            let installed = file_sha256_hex(&pending.target).ok();
            // A crash between rename and requesting exit is recoverable without executing the
            // replacement twice. Only the new process is allowed to claim running success.
            if installed.as_deref() == Some(&pending.offer.sha256)
                && running != pending.offer.sha256
            {
                return Ok(true);
            }
            pending.report = Some(result_report(
                &pending.offer,
                running,
                installed,
                true,
                None,
            )?);
            write_pending(options, &pending)?;
        }
        send_pending(options, &pending)?;
        return Ok(false);
    }
    let Some(offer) = fetch_offer(options)? else {
        return Ok(false);
    };
    validate_offer(&offer)?;
    let target = self_path()?;
    let tracked = offer.release_id.is_some();
    if offer.sha256 == running {
        // Bootstrap from a pre-ledger Agent has no durable update receipt. The Console labels
        // this as observed confirmation and requires fresh post-dispatch runtime evidence.
        if tracked {
            let pending = PendingUpdate {
                report: Some(result_report(
                    &offer,
                    running,
                    file_sha256_hex(&target).ok(),
                    false,
                    None,
                )?),
                offer,
                target,
            };
            write_pending(options, &pending)?;
            send_pending(options, &pending)?;
        }
        return Ok(false);
    }
    let download = options.state_dir.join(DOWNLOAD_FILE);
    let source = BinarySource {
        url: offer.url.clone(),
        sha256: offer.sha256.clone(),
    };
    let guard = || {
        if let Some(previous) = &offer.previous_sha256 {
            if previous != running || file_sha256_hex(&target)? != *previous {
                return Err("Agent 本地摘要已偏离批准时状态，未替换".to_owned());
            }
        }
        if tracked {
            let current = fetch_offer(options)?;
            if current.as_ref() != Some(&offer) {
                return Err("Agent 发布已停止或尝试编号已变化，未替换".to_owned());
            }
            write_pending(
                options,
                &PendingUpdate {
                    offer: offer.clone(),
                    target: target.clone(),
                    report: None,
                },
            )?;
        }
        Ok(())
    };
    let outcome = install_release(&source, &offer.sha256, &download, &target, guard);
    let _ = fs::remove_file(&download);
    if let Err(error) = outcome {
        if tracked && file_sha256_hex(&target).ok().as_deref() == Some(&offer.sha256) {
            // Rename committed but directory fsync failed. Keep the prepared receipt and let
            // the new process prove the actual state, rather than overwrite it with a failure.
            warn(format!("selfupdate: 替换已完成，持久化确认有异常：{error}"));
            return Ok(true);
        }
        if tracked {
            let pending = PendingUpdate {
                report: Some(result_report(
                    &offer,
                    running,
                    file_sha256_hex(&target).ok(),
                    false,
                    Some(error.clone()),
                )?),
                offer,
                target,
            };
            write_pending(options, &pending)?;
            send_pending(options, &pending)?;
        }
        return Err(error);
    }
    println!(
        "selfupdate: 已安装 {}，等待安全退出及新进程确认",
        short(&offer.sha256)
    );
    Ok(true)
}

fn fetch_offer(options: &Options) -> Result<Option<AgentReleaseOffer>, String> {
    let response = HttpClient::new(&options.server)?.request_with_headers(
        "GET",
        "/agent/v1/agent-release",
        &options.token,
        None,
        &[
            (ARCH_HEADER, identity::self_arch().to_owned()),
            ("x-brocade-agent-release-receipt", "1".to_owned()),
        ],
    )?;
    if response.status == 204 {
        return Ok(None);
    }
    if !(200..300).contains(&response.status) {
        return Err(format!("Agent 发布查询失败：HTTP {}", response.status));
    }
    serde_json::from_str(&response.body)
        .map(Some)
        .map_err(|e| format!("Agent 发布响应无效：{e}"))
}

fn valid_digest(sha: &str) -> bool {
    sha.len() == 64
        && sha
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn validate_offer(offer: &AgentReleaseOffer) -> Result<(), String> {
    if !valid_digest(&offer.sha256) {
        return Err("Agent 目标摘要无效".to_owned());
    }
    match (
        offer.release_id,
        offer.attempt,
        offer.previous_sha256.as_deref(),
    ) {
        (None, None, None) => Ok(()), // old Console
        (Some(id), Some(attempt), Some(before))
            if id > 0 && (1..=32).contains(&attempt) && valid_digest(before) =>
        {
            Ok(())
        }
        _ => Err("Agent 发布单身份不完整".to_owned()),
    }
}

fn read_pending(path: &Path) -> Result<Option<PendingUpdate>, String> {
    use std::io::Read;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读取 Agent 发布回执失败：{e}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PENDING_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PENDING_BYTES {
        return Err("Agent 发布回执超出大小限制".to_owned());
    }
    let pending: PendingUpdate =
        serde_json::from_slice(&bytes).map_err(|e| format!("Agent 发布回执无效：{e}"))?;
    validate_offer(&pending.offer)?;
    if pending.offer.release_id.is_none() || !pending.target.is_absolute() {
        return Err("Agent 发布回执缺少发布身份或安装路径".to_owned());
    }
    if pending.report.as_ref().is_some_and(|report| {
        Some(report.release_id) != pending.offer.release_id
            || Some(report.attempt) != pending.offer.attempt
    }) {
        return Err("Agent 发布回执与批准身份不一致".to_owned());
    }
    Ok(Some(pending))
}

fn write_pending(options: &Options, pending: &PendingUpdate) -> Result<(), String> {
    let encoded = serde_json::to_vec(pending).map_err(|e| e.to_string())?;
    crate::fsutil::atomic_write_private(&options.state_dir.join(PENDING_FILE), &encoded)
}

fn result_report(
    offer: &AgentReleaseOffer,
    running: &str,
    installed: Option<String>,
    performed_update: bool,
    error: Option<String>,
) -> Result<AgentReleaseReport, String> {
    let healthy =
        error.is_none() && running == offer.sha256 && installed.as_deref() == Some(running);
    Ok(AgentReleaseReport {
        release_id: offer.release_id.ok_or("missing Agent release id")?,
        attempt: offer.attempt.ok_or("missing Agent release attempt")?,
        outcome: if healthy {
            AgentReleaseOutcome::Running
        } else {
            AgentReleaseOutcome::Failed
        },
        performed_update,
        installed_sha256: installed,
        running_sha256: Some(running.to_owned()),
        error: if healthy {
            None
        } else {
            Some(
                error
                    .unwrap_or_else(|| "重启后未运行目标 Agent".to_owned())
                    .chars()
                    .take(4_000)
                    .collect(),
            )
        },
    })
}

fn send_pending(options: &Options, pending: &PendingUpdate) -> Result<(), String> {
    let report = pending
        .report
        .as_ref()
        .ok_or("Agent replacement has not been confirmed")?;
    let body = serde_json::to_string(report).map_err(|e| e.to_string())?;
    let response = HttpClient::new(&options.server)?.request(
        "POST",
        "/agent/v1/agent-release/report",
        &options.token,
        Some(&body),
    )?;
    if !(200..300).contains(&response.status) {
        return Err(format!("Agent 发布回执待确认：HTTP {}", response.status));
    }
    #[derive(Deserialize)]
    struct Ack {
        accepted: bool,
    }
    let ack: Ack =
        serde_json::from_str(&response.body).map_err(|e| format!("Agent 回执响应无效：{e}"))?;
    if !ack.accepted {
        println!("selfupdate: 本次回执已过期，停止补报");
    }
    fs::remove_file(options.state_dir.join(PENDING_FILE))
        .map_err(|e| format!("清理 Agent 回执失败：{e}"))?;
    Ok(())
}

fn install_release(
    release: &BinarySource,
    wanted: &str,
    download: &Path,
    target: &Path,
    before_replace: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    fetch(&release.url, download)?;

    let got = file_sha256_hex(download)?;
    if got != wanted {
        // Both values named. "sha mismatch" alone leaves nobody able to tell a truncated download
        // from a control plane serving something other than what it announced.
        return Err(format!(
            "下载下来的 agent sha256 对不上：要 {wanted}，实际 {got}。没有安装"
        ));
    }

    // Executable before it can be tried, and 0755 rather than 0700 because this is what the
    // installed binary's mode will be — testing it under a different mode tests something else.
    fs::set_permissions(download, permissions(0o755))
        .map_err(|error| format!("给候选 agent 加执行位失败: {error}"))?;
    probe(download)?;

    // Staged beside the target rather than renamed from the state directory: `rename` cannot cross
    // filesystems, and `/var/lib` and `/usr/local/bin` are routinely on different ones.
    let staging = target
        .parent()
        .ok_or_else(|| format!("agent 路径没有上级目录: {}", target.display()))?
        .join(STAGING_FILE);
    fs::copy(download, &staging).map_err(|error| {
        format!(
            "拷到 {} 失败: {error}（这个目录得可写才能自更新）",
            staging.display()
        )
    })?;
    if let Err(error) = fs::set_permissions(&staging, permissions(0o755)) {
        let _ = fs::remove_file(&staging);
        return Err(format!("给暂存的 agent 加执行位失败: {error}"));
    }
    // The only irreversible step, and it is atomic: readers of this path either see the whole old
    // file or the whole new one. The running process keeps the old inode regardless.
    fs::File::open(&staging)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("同步 Agent 暂存文件失败：{e}"))?;
    if let Err(error) = before_replace() {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    fs::rename(&staging, target).map_err(|error| {
        let _ = fs::remove_file(&staging);
        format!("换 {} 失败: {error}", target.display())
    })?;
    if let Some(parent) = target.parent() {
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| format!("同步 Agent 安装目录失败：{e}"))?;
    }
    Ok(())
}

/// Download with curl rather than the agent's own HTTP client.
///
/// The client parses a response into a `String`, and a binary put through UTF-8 conversion is a
/// corrupted binary that then fails the sha check with nothing to point at. curl is already a hard
/// dependency of `install.sh` and of the phantun fetch path, so this adds no new requirement.
fn fetch(url: &str, into: &Path) -> Result<(), String> {
    run_shell_with_timeout(
        &format!(
            "curl -fsSL --connect-timeout 15 --max-time 300 --max-filesize 134217728 {} -o {}",
            shell_quote(url),
            shell_quote(&into.to_string_lossy())
        ),
        Duration::from_secs(310),
    )
    .map(|_| ())
    .map_err(|error| format!("下载 agent 失败：{error}"))
}

/// Run the candidate once, before it replaces anything.
///
/// The test is that it can execute and knows the subcommand the service starts it with. It is fed a
/// command that does not exist, because that makes it print what it does support without doing
/// anything: `--server` and `--token` are supplied only because argument validation runs before
/// command dispatch, and this points at a port nothing listens on.
///
/// `install.sh` gates on the same output for the same reason; the two must keep agreeing, or a
/// binary the installer would reject could still arrive this way.
fn probe(candidate: &Path) -> Result<(), String> {
    let candidate = candidate.to_string_lossy();
    let out = capture_command_with_timeout(
        &candidate,
        &[
            "--server",
            "http://127.0.0.1:1",
            "--token",
            "probe",
            "__brocade_probe",
        ],
        Duration::from_secs(10),
    )
    .map_err(|error| {
        format!(
            "候选 agent 跑不起来（{error}）——多半不是本机架构（{}），没有安装",
            identity::self_arch()
        )
    })?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !said.contains("expected") || !said.contains("run") {
        return Err(format!(
            "候选 agent 不认识 run 子命令，装上去服务也起不来，没有安装。它说：{}",
            said.trim()
        ));
    }
    Ok(())
}

/// The path of the running binary.
///
/// Read through `/proc/self/exe` rather than `argv[0]`, which is whatever the caller chose and is
/// possibly a relative path under a service manager. A path ending in ` (deleted)` means the file
/// was already replaced and this process is the old one still running — there is nothing left to
/// update, and writing to that literal path would create a file with a space and `(deleted)` in its
/// name.
fn self_path() -> Result<PathBuf, String> {
    let path = fs::read_link("/proc/self/exe")
        .map_err(|error| format!("读不了 /proc/self/exe: {error}"))?;
    if path.to_string_lossy().ends_with(" (deleted)") {
        return Err("这个进程的二进制已经被换掉了，等它退出即可".to_owned());
    }
    Ok(path)
}

fn permissions(mode: u32) -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    fs::Permissions::from_mode(mode)
}

/// Enough of a sha256 to recognize in a log line, in git's spelling. The full value is what gets
/// compared and what the console shows.
fn short(sha256: &str) -> &str {
    &sha256[..sha256.len().min(12)]
}

/// The loop.
///
/// Its own thread, at its own cadence. Not folded into the runtime-report thread even though both
/// are low-frequency and both concern versions: that one only observes, while this one can end the
/// process, and a panic or a stall in one should not take the other with it.
///
/// Ten minutes rather than the half-hour used for reporting. The number is set by how long an
/// operator waits watching the first node of a staged rollout, not by how much the machine can
/// afford — the request is one line and answers 204 nearly every time.
pub(crate) fn spawn_selfupdate(options: &Options, wants_exit: &Arc<AtomicBool>) {
    let options = options.clone();
    let wants_exit = Arc::clone(wants_exit);
    let spawned = std::thread::Builder::new()
        .name("selfupdate".to_owned())
        .spawn(move || loop {
            crate::each_round("selfupdate", || match selfupdate_cycle(&options) {
                Ok(true) => wants_exit.store(true, Ordering::SeqCst),
                Ok(false) => {}
                Err(error) => warn(format!("selfupdate: {error}")),
            });
            if wants_exit.load(Ordering::SeqCst) {
                break;
            }
            let interval = if options.state_dir.join(PENDING_FILE).exists() {
                Duration::from_secs(15)
            } else {
                SELFUPDATE_INTERVAL
            };
            std::thread::sleep(interval);
        });
    if let Err(error) = spawned {
        // Not fatal. A machine that cannot self-update still converges, still reports, and still
        // carries traffic; install.sh remains the manual recovery path.
        warn(format!(
            "selfupdate: 起不了自更新线程（{error}）；这台机器只能靠 install.sh 升级"
        ));
    }
}

pub(crate) const SELFUPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked_offer() -> AgentReleaseOffer {
        AgentReleaseOffer {
            url: "https://console.example/agent".to_owned(),
            sha256: "a".repeat(64),
            release_id: Some(7),
            attempt: Some(2),
            previous_sha256: Some("b".repeat(64)),
        }
    }

    #[test]
    fn replacement_is_not_success_until_the_new_process_is_running() {
        let offer = tracked_offer();
        let before = result_report(
            &offer,
            &"b".repeat(64),
            Some(offer.sha256.clone()),
            true,
            None,
        )
        .unwrap();
        assert_eq!(before.outcome, AgentReleaseOutcome::Failed);
        let after = result_report(
            &offer,
            &offer.sha256,
            Some(offer.sha256.clone()),
            true,
            None,
        )
        .unwrap();
        assert_eq!(after.outcome, AgentReleaseOutcome::Running);
        assert_eq!((after.release_id, after.attempt), (7, 2));
        let observed = result_report(
            &offer,
            &offer.sha256,
            Some(offer.sha256.clone()),
            false,
            None,
        )
        .unwrap();
        assert!(
            !observed.performed_update,
            "legacy bootstrap must not invent a receipt"
        );
    }

    #[test]
    fn old_console_offer_is_supported_but_partial_tracking_is_rejected() {
        let mut offer = tracked_offer();
        assert!(validate_offer(&offer).is_ok());
        offer.attempt = None;
        assert!(validate_offer(&offer).is_err());
        offer.release_id = None;
        offer.previous_sha256 = None;
        assert!(validate_offer(&offer).is_ok());
        offer.sha256 = "A".repeat(64);
        assert!(validate_offer(&offer).is_err());
    }

    #[test]
    fn prepared_update_survives_a_restart_without_fabricating_a_report() {
        let dir =
            std::env::temp_dir().join(format!("brocade-agent-receipt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(PENDING_FILE);
        let pending = PendingUpdate {
            offer: tracked_offer(),
            target: PathBuf::from("/usr/local/bin/brocade-agent"),
            report: None,
        };
        crate::fsutil::atomic_write_private(&path, &serde_json::to_vec(&pending).unwrap()).unwrap();
        let restored = read_pending(&path).unwrap().unwrap();
        assert_eq!(restored.offer, pending.offer);
        assert!(restored.report.is_none());
        fs::write(&path, vec![0; MAX_PENDING_BYTES as usize + 1]).unwrap();
        assert!(read_pending(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn short_is_twelve_hex_and_survives_a_short_input() {
        assert_eq!(short(&"a".repeat(64)), "aaaaaaaaaaaa");
        assert_eq!(short("abc"), "abc");
    }

    /// The candidate is run before it is installed, and the whole point is that a file which
    /// cannot execute never reaches the rename.
    #[test]
    fn a_candidate_that_cannot_execute_is_refused() {
        let dir = std::env::temp_dir().join(format!("brocade-selfupdate-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("not-a-binary");
        fs::write(&junk, b"#!/nonexistent\n").unwrap();
        fs::set_permissions(&junk, permissions(0o755)).unwrap();
        assert!(probe(&junk).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// The test binary answers `--help`-ish output that has neither word, so it stands in for a
    /// binary that runs but is not this program.
    #[test]
    fn a_binary_that_runs_but_is_not_an_agent_is_refused() {
        assert!(probe(Path::new("/bin/true")).is_err());
    }

    /// The order the whole design rests on: verify, then execute, then install.
    ///
    /// Driven through the real `fetch` over a `file://` URL, so the download path runs rather than
    /// being stubbed out. What is asserted is not only the error but that **the target was never
    /// touched** — a version that verified after installing would pass an error-only assertion
    /// while having already replaced the binary on the machine.
    #[test]
    fn a_sha_that_does_not_match_stops_before_anything_is_installed() {
        let dir = std::env::temp_dir().join(format!("brocade-sha-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("candidate");
        fs::write(&source, b"whatever bytes").unwrap();

        let target = dir.join("brocade-agent");
        fs::write(&target, b"the binary in use").unwrap();
        let download = dir.join(DOWNLOAD_FILE);

        let release = BinarySource {
            url: format!("file://{}", source.display()),
            sha256: "f".repeat(64),
        };
        let error = install_release(&release, &"f".repeat(64), &download, &target, || Ok(()))
            .expect_err("sha 对不上就不该装");
        assert!(error.contains("对不上"), "{error}");

        assert_eq!(
            fs::read(&target).unwrap(),
            b"the binary in use",
            "校验没过却动了正在用的二进制"
        );
        // And nothing was staged next to it either.
        assert!(!dir.join(STAGING_FILE).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn canceled_assignment_cannot_replace_binary_and_cleans_staging() {
        let dir = std::env::temp_dir().join(format!("brocade-agent-fence-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("candidate");
        fs::write(&source, b"#!/bin/sh\nprintf 'expected run\\n'\n").unwrap();
        let target = dir.join("brocade-agent");
        fs::write(&target, b"old binary").unwrap();
        let wanted = file_sha256_hex(&source).unwrap();
        let release = BinarySource {
            url: format!("file://{}", source.display()),
            sha256: wanted.clone(),
        };
        let error = install_release(&release, &wanted, &dir.join(DOWNLOAD_FILE), &target, || {
            Err("assignment canceled".to_owned())
        })
        .unwrap_err();
        assert_eq!(error, "assignment canceled");
        assert_eq!(fs::read(&target).unwrap(), b"old binary");
        assert!(!dir.join(STAGING_FILE).exists());
        install_release(&release, &wanted, &dir.join(DOWNLOAD_FILE), &target, || {
            Ok(())
        })
        .unwrap();
        assert_eq!(file_sha256_hex(&target).unwrap(), wanted);
        fs::remove_dir_all(dir).unwrap();
    }
}
