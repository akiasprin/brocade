//! Bounded execution for every external command the agent invokes.
//!
//! A command is part of convergence, not an independent service: once it stops
//! making progress it must give the agent its thread and locks back.  Shell
//! commands get their own process group so a timeout also reaches grandchildren
//! such as `curl`, not merely the outer `sh`.
use std::{
    io::Read,
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const TERM_GRACE: Duration = Duration::from_millis(200);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) struct CapturedOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

pub(crate) fn run_command(program: &str, args: &[&str]) -> Result<String, String> {
    run_command_with_timeout(program, args, DEFAULT_COMMAND_TIMEOUT)
}

pub(crate) fn run_shell(script: &str) -> Result<String, String> {
    run_shell_with_timeout(script, DEFAULT_COMMAND_TIMEOUT)
}

pub(crate) fn run_shell_with_timeout(script: &str, timeout: Duration) -> Result<String, String> {
    run_command_with_timeout("sh", &["-c", script], timeout)
}

pub(crate) fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let output = capture_command_with_timeout(program, args, timeout)?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(format!(
        "{} {} failed with {}: {}{}{}",
        program,
        args.join(" "),
        output.status,
        stdout.trim(),
        if stdout.trim().is_empty() || stderr.trim().is_empty() {
            ""
        } else {
            "\n"
        },
        stderr.trim()
    ))
}

/// The lower-level form used when non-zero is expected and its output still has
/// meaning, as with probing a candidate agent using an intentionally unknown
/// subcommand.
pub(crate) fn capture_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<CapturedOutput, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // `0` makes the child the leader of a fresh process group.  Killing only
        // `sh` leaves its `curl`/`wg` grandchild running and, because that child
        // still owns the output pipes, leaves the reader threads waiting too.
        .process_group(0)
        .spawn()
        .map_err(|error| format!("failed to run {program}: {error}"))?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    // Read concurrently with waiting.  Waiting first deadlocks when a noisy
    // command fills a kernel pipe buffer before it exits.
    let stdout_reader = thread::spawn(move || read_all(stdout));
    let stderr_reader = thread::spawn(move || read_all(stderr));

    let waited = wait_bounded(&mut child, timeout);
    let stdout = join_reader(stdout_reader, program, "stdout")?;
    let stderr = join_reader(stderr_reader, program, "stderr")?;
    let status = waited.map_err(|error| format!("{} {}: {error}", program, args.join(" ")))?;

    Ok(CapturedOutput {
        status,
        stdout,
        stderr,
    })
}

/// For probes where only the exit status matters.  It deliberately shares the
/// same bounded wait as captured commands; otherwise one wedged `pgrep` or `ip`
/// still freezes health/self-heal despite `run_command` being safe.
pub(crate) fn command_success(program: &str, args: &[&str]) -> bool {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    else {
        return false;
    };
    wait_bounded(&mut child, DEFAULT_COMMAND_TIMEOUT).is_ok_and(|status| status.success())
}

fn read_all(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_reader(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    program: &str,
    stream: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("{program}: {stream} reader panicked"))?
        .map_err(|error| format!("{program}: failed to read {stream}: {error}"))
}

fn wait_bounded(child: &mut Child, timeout: Duration) -> Result<ExitStatus, String> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
            }
            Ok(None) => {
                terminate_process_group(child);
                return Err(format!(
                    "command pid {} timed out after {:.3}s",
                    child.id(),
                    timeout.as_secs_f64()
                ));
            }
            Err(error) => {
                terminate_process_group(child);
                return Err(format!("failed waiting for pid {}: {error}", child.id()));
            }
        }
    }
}

fn terminate_process_group(child: &mut Child) {
    let pgid = -(child.id() as libc::pid_t);
    // SAFETY: the child was placed in a new process group whose id is its pid.
    // ESRCH merely means it exited between try_wait and kill.
    unsafe {
        libc::kill(pgid, libc::SIGTERM);
    }
    thread::sleep(TERM_GRACE);
    // Always send SIGKILL to the group.  The leader may have exited on SIGTERM
    // while a grandchild ignored it and still owns our stdout/stderr pipes.
    unsafe {
        libc::kill(pgid, libc::SIGKILL);
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{run_command, run_shell_with_timeout};

    #[test]
    fn captures_stdout_without_waiting_for_a_pipe_to_fill() {
        let output = run_command("sh", &["-c", "yes x | head -n 50000"]).unwrap();
        assert!(output.len() > 50_000);
    }

    #[test]
    fn timeout_kills_the_shell_process_group() {
        let started = Instant::now();
        let error =
            run_shell_with_timeout("trap '' TERM; sleep 30 & wait", Duration::from_millis(50))
                .unwrap_err();

        assert!(error.contains("timed out"), "实际错误：{error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "超时后没有及时拿回执行权"
        );
    }
}
