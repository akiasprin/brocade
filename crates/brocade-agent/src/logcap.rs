//! A tiny bounded log sink for workloads launched by the agent.
//!
//! Deleting an open `/tmp/xray.log` unlinks the name but does not release its blocks until Xray
//! exits. `copytruncate` has a race, and relying on a distro's hourly logrotate leaves a noisy
//! process free to fill the disk in between. The sink owns the file descriptor and rotates before
//! crossing the bound, so the invariant is local and continuous.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use brocade_deployment::protocol::{
    DEFAULT_AGENT_LOG_MAX_MIB, MAX_AGENT_LOG_MAX_MIB, MIN_AGENT_LOG_MAX_MIB,
};

const LOG_POLICY_FILE: &str = "log-max-mib";
const AGENT_JOURNAL_DROPIN: &str =
    "/etc/systemd/system/brocade-agent.service.d/20-log-namespace.conf";
const AGENT_JOURNAL_CONFIG: &str = "/etc/systemd/journald@brocade-agent.conf.d/limits.conf";
const AGENT_JOURNAL_DROPIN_CONTENT: &[u8] = b"[Service]\nLogNamespace=brocade-agent\n";

fn max_bytes(max_mib: u32) -> u64 {
    u64::from(max_mib) * 1024 * 1024
}

fn validate_mib(max_mib: u32) -> Result<u32, String> {
    if !(MIN_AGENT_LOG_MAX_MIB..=MAX_AGENT_LOG_MAX_MIB).contains(&max_mib) {
        return Err(format!(
            "log max must be {MIN_AGENT_LOG_MAX_MIB}..={MAX_AGENT_LOG_MAX_MIB} MiB"
        ));
    }
    Ok(max_mib)
}

fn policy_path(state_dir: &Path) -> PathBuf {
    state_dir.join(LOG_POLICY_FILE)
}

fn read_policy_mib(path: &Path) -> Option<u32> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
        .and_then(|value| validate_mib(value).ok())
}

fn journal_policy(max_mib: u32) -> Vec<u8> {
    // One journal file is capped at a quarter of the namespace, retaining several independently
    // searchable segments without letting metadata consume most of a very small policy.
    let file_mib = (max_mib / 4).max(1);
    format!(
        "[Journal]\nStorage=persistent\nSystemMaxUse={max_mib}M\nRuntimeMaxUse={max_mib}M\nSystemMaxFileSize={file_mib}M\nRuntimeMaxFileSize={file_mib}M\n"
    )
    .into_bytes()
}

/// Existing nodes normally receive a new agent through self-update, not by rerunning install.sh.
/// Ensure that path gains the same per-agent journal namespace. `true` asks the resident process
/// to exit once; Restart=always then starts it under the newly loaded unit property.
pub(crate) fn ensure_agent_journal_namespace(state_dir: &Path) -> Result<bool, String> {
    if std::env::var_os("INVOCATION_ID").is_none() || !Path::new("/run/systemd/system").exists() {
        return Ok(false);
    }
    let version = crate::run_command("systemctl", &["--version"])?;
    if systemd_version(&version).is_none_or(|version| version < 245) {
        return Ok(false);
    }

    let dropin = Path::new(AGENT_JOURNAL_DROPIN);
    let config = Path::new(AGENT_JOURNAL_CONFIG);
    let max_mib = read_policy_mib(&policy_path(state_dir)).unwrap_or(DEFAULT_AGENT_LOG_MAX_MIB);
    let dropin_changed = write_if_changed(dropin, AGENT_JOURNAL_DROPIN_CONTENT)?;
    let config_changed = write_if_changed(config, &journal_policy(max_mib))?;
    if !dropin_changed && !config_changed {
        return Ok(false);
    }
    if dropin_changed {
        crate::run_command("systemctl", &["daemon-reload"])?;
    }
    // Inactive is success for try-restart. A live namespace is restarted to read the new ceiling;
    // on the next agent start LogNamespace= creates it when it did not exist yet.
    let _ = crate::run_command(
        "systemctl",
        &["try-restart", "systemd-journald@brocade-agent.service"],
    );
    // Only installing/changing LogNamespace requires this agent process to restart. A changed
    // journal ceiling is picked up by restarting journald and leaves convergence running.
    Ok(dropin_changed)
}

/// Persist and apply the resolved value from the control plane. Xray and Phantun sinks poll this
/// file once a second while running, including while their input is quiet, so changing the ceiling
/// neither restarts a workload nor races an external process truncating its open descriptor.
pub(crate) fn apply_policy(state_dir: &Path, max_mib: u32) -> Result<bool, String> {
    let max_mib = validate_mib(max_mib)?;
    fs::create_dir_all(state_dir)
        .map_err(|error| format!("create state directory {}: {error}", state_dir.display()))?;
    let path = policy_path(state_dir);
    let contents = format!("{max_mib}\n");
    let changed = fs::read(&path).ok().as_deref() != Some(contents.as_bytes());
    if changed {
        crate::fsutil::atomic_write_private(&path, contents.as_bytes())?;
    }

    if std::env::var_os("INVOCATION_ID").is_some() && Path::new("/run/systemd/system").exists() {
        let journal = journal_policy(max_mib);
        // A previous round may have written the policy file and then failed before updating
        // journald. Compare the cheap local file every round and retry only while it differs;
        // after convergence this path forks no systemctl process on the 15-second poll.
        if fs::read(AGENT_JOURNAL_CONFIG).ok().as_deref() != Some(journal.as_slice()) {
            let version = crate::run_command("systemctl", &["--version"])?;
            if systemd_version(&version).is_some_and(|version| version >= 245) {
                let config_changed = write_if_changed(Path::new(AGENT_JOURNAL_CONFIG), &journal)?;
                if config_changed {
                    let _ = crate::run_command(
                        "systemctl",
                        &["try-restart", "systemd-journald@brocade-agent.service"],
                    );
                }
            }
        }
    }
    Ok(changed)
}

fn systemd_version(output: &str) -> Option<u32> {
    output
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<bool, String> {
    if fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "create journal policy directory {}: {error}",
            parent.display()
        )
    })?;
    crate::fsutil::atomic_write_private(path, contents)?;
    Ok(true)
}

pub(crate) fn run_args(args: &[String]) -> Result<(), String> {
    if args.len() != 2 {
        return Err("usage: brocade-agent log-sink PATH POLICY_FILE".to_owned());
    }
    // The sink intentionally survives an Agent service restart alongside Xray. Leaving its comm
    // as `brocade-agent` makes the process sampler select this older helper instead of the new
    // resident Agent, corrupting RSS/CPU/fd attribution. Linux truncates comm at 15 bytes; this
    // distinct short name keeps it out of WATCHED without relying on argv parsing.
    let _ = fs::write("/proc/self/comm", b"brocade-log\n");
    let mut input = io::stdin().lock();
    if let Err(error) = consume_dynamic(&mut input, Path::new(&args[0]), Path::new(&args[1])) {
        // Logging must fail open. Exiting closes the pipe and can deliver SIGPIPE to Xray — a full
        // log disk would then become a traffic outage. Report once and keep draining to /dev/null;
        // the disk telemetry still exposes the underlying capacity problem.
        eprintln!("log sink {} disabled: {error}", args[0]);
        io::copy(&mut input, &mut io::sink())
            .map_err(|drain| format!("log sink {} drain failed: {drain}", args[0]))?;
    }
    Ok(())
}

/// Shell fragment naming this exact agent binary as a sink. It remains valid across an atomic
/// self-update: an existing sink keeps its mapped inode, and a newly spawned workload resolves the
/// replacement at the same path.
pub(crate) fn command(path: &Path, state_dir: &Path) -> Result<String, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate agent binary for log sink: {error}"))?;
    Ok(format!(
        "{} log-sink {} {}",
        crate::shell_quote(&executable.display().to_string()),
        crate::shell_quote(&path.display().to_string()),
        crate::shell_quote(&policy_path(state_dir).display().to_string())
    ))
}

#[cfg(test)]
fn consume(input: &mut impl Read, path: &Path, max_bytes: u64) -> io::Result<()> {
    consume_with_limit(input, path, max_bytes, || max_bytes, || Ok(true))
}

fn consume_dynamic(input: &mut impl Read, path: &Path, policy: &Path) -> io::Result<()> {
    let initial = read_policy_mib(policy)
        .map(max_bytes)
        .unwrap_or_else(|| max_bytes(DEFAULT_AGENT_LOG_MAX_MIB));
    consume_with_limit(
        input,
        path,
        initial,
        || read_policy_mib(policy).map(max_bytes).unwrap_or(initial),
        stdin_ready,
    )
}

fn consume_with_limit(
    input: &mut impl Read,
    path: &Path,
    initial_max_bytes: u64,
    mut current_limit: impl FnMut() -> u64,
    mut input_ready: impl FnMut() -> io::Result<bool>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let archive = archive_path(path);
    let mut max_bytes = initial_max_bytes;
    let mut segment_bytes = max_bytes / 2;
    normalize_segment(&archive, segment_bytes)?;

    // A legacy unbounded current file becomes the predecessor. Starting a fresh current segment
    // guarantees that bytes arriving after the upgrade are retained, instead of immediately
    // deleting them at the next boundary.
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= segment_bytes)
    {
        compact_tail(path, segment_bytes)?;
        remove_if_present(&archive)?;
        fs::rename(path, &archive)?;
    }

    let mut output = open_append(path)?;
    let mut written = output.metadata()?.len();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let next_max_bytes = current_limit();
        if next_max_bytes != max_bytes && next_max_bytes >= 2 {
            output.flush()?;
            drop(output);
            max_bytes = next_max_bytes;
            segment_bytes = max_bytes / 2;
            normalize_segment(path, segment_bytes)?;
            normalize_segment(&archive, segment_bytes)?;
            output = open_append(path)?;
            written = output.metadata()?.len();
        }
        // A timeout is still useful: it let the limit refresh above run while the workload is
        // quiet. The sink owns the descriptor, so compaction never races another writer.
        if !input_ready()? {
            continue;
        }
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return output.flush();
        }
        let mut offset = 0;
        while offset < count {
            if written >= segment_bytes {
                output.flush()?;
                drop(output);
                remove_if_present(&archive)?;
                fs::rename(path, &archive)?;
                output = open_append(path)?;
                written = 0;
            }
            let room = (segment_bytes - written) as usize;
            let take = room.min(count - offset);
            output.write_all(&buffer[offset..offset + take])?;
            written += take as u64;
            offset += take;
        }
    }
}

fn stdin_ready() -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `descriptor` points to one initialized pollfd for the duration of the call.
        let result = unsafe { libc::poll(&mut descriptor, 1, 1_000) };
        if result > 0 {
            return Ok(true);
        }
        if result == 0 {
            return Ok(false);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn archive_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".1");
    PathBuf::from(value)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn normalize_segment(path: &Path, limit: u64) -> io::Result<()> {
    if path.metadata().is_ok_and(|metadata| metadata.len() > limit) {
        compact_tail(path, limit)?;
    }
    Ok(())
}

/// Keep the newest `limit` bytes without allocating a second file of that size. Copying towards a
/// lower offset is safe in forward order: every source byte lies strictly after its destination,
/// so a write never overwrites the next unread source range.
fn compact_tail(path: &Path, limit: u64) -> io::Result<()> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    if len <= limit {
        return Ok(());
    }
    let source = len - limit;
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while copied < limit {
        let count = (limit - copied).min(buffer.len() as u64) as usize;
        let got = file.read_at(&mut buffer[..count], source + copied)?;
        if got == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "log changed while compacting",
            ));
        }
        file.write_all_at(&buffer[..got], copied)?;
        copied += got as u64;
    }
    file.set_len(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("brocade-logcap-{name}-{}", std::process::id()))
    }

    #[test]
    fn current_and_archive_never_exceed_the_total_bound() {
        let dir = temp("bound");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("xray.log");
        let input = (0_u8..64).collect::<Vec<_>>();
        consume(&mut input.as_slice(), &path, 20).unwrap();

        let current = fs::read(&path).unwrap();
        let archive = fs::read(archive_path(&path)).unwrap();
        assert!(current.len() + archive.len() <= 20);
        let mut retained = archive;
        retained.extend(current);
        assert!(
            input.ends_with(&retained),
            "only the newest complete segments remain"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_oversized_legacy_file_is_bounded_before_new_bytes_arrive() {
        let dir = temp("legacy");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("xray.log");
        fs::write(&path, b"0123456789ABCDEFGHIJ").unwrap();
        consume(&mut &b"new"[..], &path, 12).unwrap();

        assert_eq!(fs::read(archive_path(&path)).unwrap(), b"EFGHIJ");
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_live_limit_reduction_compacts_both_segments_before_writing() {
        let dir = temp("dynamic-lower");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("xray.log");
        fs::write(&path, b"abcdefghij").unwrap();
        fs::write(archive_path(&path), b"0123456789").unwrap();
        let mut limits = [12_u64].into_iter();
        consume_with_limit(
            &mut &b"XYZ"[..],
            &path,
            20,
            || limits.next().unwrap_or(12),
            || Ok(true),
        )
        .unwrap();

        let current = fs::read(&path).unwrap();
        let archive = fs::read(archive_path(&path)).unwrap();
        assert!(current.len() <= 6);
        assert!(archive.len() <= 6);
        assert!(current.len() + archive.len() <= 12);
        assert!(current.ends_with(b"XYZ"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn journal_file_ceiling_tracks_a_quarter_of_machine_policy() {
        let rendered = String::from_utf8(journal_policy(320)).unwrap();
        assert!(rendered.contains("SystemMaxUse=320M"));
        assert!(rendered.contains("SystemMaxFileSize=80M"));
    }

    #[test]
    fn systemd_version_is_read_without_depending_on_distribution_suffixes() {
        assert_eq!(
            systemd_version("systemd 252 (252.38-1~deb12u1)\n+PAM"),
            Some(252)
        );
        assert_eq!(systemd_version("systemd 245\n"), Some(245));
        assert_eq!(systemd_version("not-systemd\n"), None);
    }
}
