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
//!    already took the release — the common case after a successful upgrade, and the reason this
//!    can run on a timer without needing to remember anything across restarts.
//! 3. Download to the state directory, which is 0700.
//! 4. Verify the sha256 before anything is executed or installed.
//! 5. **Run the candidate once** before it replaces anything. This is what stops a binary that
//!    cannot execute on this machine — wrong architecture, truncated download, a libc assumption
//!    that does not hold here — from becoming the installed one. `install.sh` gates on the same
//!    signal for the same reason.
//! 6. Stage next to the real binary and `rename` over it. Not `install`(1) and not writing in
//!    place: opening a running executable for writing returns `ETXTBSY`. `rename` is atomic, and
//!    the running process keeps its own inode until it exits.
//! 7. Ask the main loop to exit. systemd's `Restart=always` starts the new one five seconds later.
//!
//! # This depends on something restarting the process
//!
//! Step 7 hands the last move to the supervisor, and the only supervisor `install.sh` writes is a
//! systemd unit. Installed with `--service-mode foreground` — which is what the preview containers
//! use (`brocade-preview`'s `provision.rs`) — nothing restarts it, so a self-update stops the agent
//! and leaves it stopped.
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

use brocade_deployment::protocol::BinarySource;

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
pub(crate) fn selfupdate_cycle(options: &Options) -> Result<bool, String> {
    let Some(running) = identity::self_sha256() else {
        // `identity` already said why, once, at startup. Self-update cannot proceed without
        // knowing what is running: every round would download and install the same binary again.
        return Ok(false);
    };

    let client = HttpClient::new(&options.server)?;
    let response = client.request_with_headers(
        "GET",
        "/agent/v1/agent-release",
        &options.token,
        None,
        &[(ARCH_HEADER, identity::self_arch().to_owned())],
    )?;
    if response.status == 204 {
        return Ok(false);
    }
    // 404 means this control plane has no such endpoint — it predates self-update. Not an error,
    // and specifically not one to repeat every cycle: the state is legitimate and lasts as long as
    // somebody leaves it there. Two ways to arrive at it, and the second is the common one:
    //
    //   - the control plane was rolled back to a build from before this feature
    //   - a machine was installed with a newer agent than the control plane running the fleet
    //
    // Treated as an error, an agent in either state files a warning into journald every ten
    // minutes forever, which trains whoever reads those logs to ignore this thread.
    if response.status == 404 {
        return Ok(false);
    }
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "问不到该装哪个 agent: HTTP {} {}",
            response.status, response.body
        ));
    }
    let release: BinarySource =
        serde_json::from_str(&response.body).map_err(|error| format!("发布响应读不懂: {error}"))?;

    let wanted = release.sha256.trim().to_ascii_lowercase();
    if wanted == running {
        // Already the released build. The control plane keeps saying so every round, and that is
        // fine — it is one small response, and it is what makes this loop stateless.
        return Ok(false);
    }
    if wanted.len() != 64 || !wanted.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("发布的 sha256 不成形: {wanted}"));
    }

    let target = self_path()?;
    let download = options.state_dir.join(DOWNLOAD_FILE);
    println!(
        "selfupdate: 控制面要求换成 {}（当前 {}），开始下载",
        short(&wanted),
        short(running)
    );

    // Failures leave nothing behind. A stale candidate is not dangerous — every round verifies
    // before it installs — but it is confusing to find, and it is a whole agent's worth of disk on
    // a machine that may not have much.
    let outcome = install_release(&release, &wanted, &download, &target);
    let _ = fs::remove_file(&download);
    outcome?;

    println!(
        "selfupdate: 已换上 {}，等这一轮收敛做完就退出，交给 systemd 拉起",
        short(&wanted)
    );
    Ok(true)
}

fn install_release(
    release: &BinarySource,
    wanted: &str,
    download: &Path,
    target: &Path,
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
    fs::rename(&staging, target).map_err(|error| {
        let _ = fs::remove_file(&staging);
        format!("换 {} 失败: {error}", target.display())
    })?;
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
            "curl -fsSL --connect-timeout 15 --max-time 300 {} -o {}",
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
/// The test is that it can execute and knows the subcommand the systemd unit starts it with. It is
/// fed a command that does not exist, because that makes it print what it does support without
/// doing anything: `--server` and `--token` are supplied only because argument validation runs
/// before command dispatch, and this points at a port nothing listens on.
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
            "候选 agent 不认识 run 子命令，装上去 systemd 也起不来，没有安装。它说：{}",
            said.trim()
        ));
    }
    Ok(())
}

/// The path of the running binary.
///
/// Read through `/proc/self/exe` rather than `argv[0]`, which is whatever the caller chose and is
/// a relative path under systemd. A path ending in ` (deleted)` means the file was already
/// replaced and this process is the old one still running — there is nothing left to update, and
/// writing to that literal path would create a file with a space and `(deleted)` in its name.
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
            std::thread::sleep(SELFUPDATE_INTERVAL);
        });
    if let Err(error) = spawned {
        // Not fatal. A machine that cannot self-update still converges, still reports, and still
        // carries traffic; it merely has to be upgraded the old way.
        warn(format!(
            "selfupdate: 起不了自更新线程（{error}）；这台机器只能靠 install.sh 升级"
        ));
    }
}

pub(crate) const SELFUPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A control plane from before self-update answers 404, and that must stay quiet.
    ///
    /// Driven against a socket that accepts and answers a canned 404, so the real request path
    /// runs. Asserted as `Ok(false)` rather than merely "no panic": the difference between this
    /// and `Err` is a warning in journald every ten minutes for as long as the fleet is in that
    /// state, which is what teaches people to stop reading this thread's logs.
    #[test]
    fn a_control_plane_without_the_endpoint_is_not_an_error() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 2048];
            let _ = socket.read(&mut buffer);
            let _ = socket.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found",
            );
        });

        let options = Options {
            command: "run".to_owned(),
            server: format!("http://127.0.0.1:{port}"),
            token: "probe".to_owned(),
            state_dir: std::env::temp_dir().join("brocade-404-test"),
            apply_mode: crate::options::ApplyMode::StateDir,
        };
        assert_eq!(selfupdate_cycle(&options), Ok(false), "404 不该报错");
        server.join().unwrap();
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
        let error = install_release(&release, &"f".repeat(64), &download, &target)
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
}
