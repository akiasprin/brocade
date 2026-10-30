//! Continuous counter identity and explicit managed downtime. A zero interval never resets a
//! cumulative byte counter. Unknown process loss is deliberately not certified as zero.

use std::{
    fs,
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use brocade_deployment::protocol::{UsageReportRequest, UsageZeroWindow};
use serde::{Deserialize, Serialize};

use crate::{
    fsutil::atomic_write_private,
    spool::{spool_push, USAGE_SPOOL},
    xray_grpc::XrayStat,
};

fn current_unix_millis() -> Result<i64, String> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    i64::try_from(elapsed.as_millis()).map_err(|_| "accounting clock overflow".to_owned())
}

pub(crate) const JOURNAL_FILE: &str = "xray-accounting.bin";
const STOP_FILE: &str = "usage-stopped.json";
const ZERO_FILE: &str = "usage-zero-windows.json";
const HEADER_SIZE: usize = 64;
const RECORD_SIZE: usize = 512;
const CAPACITY: usize = 16384;
const JOURNAL_SIZE: u64 = (HEADER_SIZE + RECORD_SIZE * CAPACITY) as u64;

#[derive(Serialize, Deserialize)]
struct StoppedUsage {
    report: UsageReportRequest,
    writer_pid: i32,
    stopped_at_unix_millis: Option<i64>,
}

pub(crate) fn journal_path(state_dir: &Path) -> PathBuf {
    state_dir.join(JOURNAL_FILE)
}

pub(crate) fn site_journal_path(state_dir: &Path) -> PathBuf {
    state_dir.join("xray-site-accounting.bin")
}

fn header(state_dir: &Path) -> Result<Option<[u8; HEADER_SIZE]>, String> {
    header_at(&journal_path(state_dir))
}

fn header_at(path: &Path) -> Result<Option<[u8; HEADER_SIZE]>, String> {
    use std::io::Read;
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read accounting journal: {error}")),
    };
    if file.metadata().map_err(|e| e.to_string())?.len() != JOURNAL_SIZE {
        return Err("invalid accounting journal size".to_owned());
    }
    let mut header = [0; HEADER_SIZE];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    if &header[..8] != b"BRACCT02" {
        return Err("invalid accounting journal format".to_owned());
    }
    Ok(Some(header))
}

pub(crate) fn epoch(state_dir: &Path, process_epoch: &str, pid: i32) -> Result<String, String> {
    let Some(header) = header(state_dir)? else {
        if process_uses_journal(state_dir, pid, None)? {
            return Err("managed accounting journal is missing; retry after recovery".to_owned());
        }
        return Ok(process_epoch.to_owned());
    };
    let writer = u32::from_le_bytes(header[28..32].try_into().map_err(|_| "journal pid")?);
    let ticks = u64::from_le_bytes(header[32..40].try_into().map_err(|_| "journal ticks")?);
    let process_ticks = process_epoch
        .rsplit(':')
        .next()
        .and_then(|ticks| ticks.parse::<u64>().ok());
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(|e| e.to_string())?;
    let boot = boot.trim().replace('-', "");
    let writer_matches = writer == pid as u32
        && process_ticks == Some(ticks)
        && boot
            == header[40..56]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
    let joined = writer != pid as u32
        && process_uses_shared_journal(state_dir, pid, b"join")?
        && crate::process_ref(writer as i32).is_some_and(|process| process.start_ticks == ticks)
        && boot
            == header[40..56]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
    if !writer_matches && !joined {
        // A joined candidate keeps the same cumulative counters after the old writer exits.
        // Until recovery promotes its identity, calling those counters a new process epoch
        // would bill their entire history again. Keep the previous checkpoint and retry.
        if process_uses_journal(state_dir, pid, None)? {
            return Err(
                "managed accounting writer is unconfirmed; retry after recovery".to_owned(),
            );
        }
        return Ok(process_epoch.to_owned());
    }
    Ok(format!(
        "journal:{}",
        header[8..24]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn process_uses_shared_journal(state_dir: &Path, pid: i32, mode: &[u8]) -> Result<bool, String> {
    process_uses_journal(state_dir, pid, Some(mode))
}

fn process_uses_journal(state_dir: &Path, pid: i32, mode: Option<&[u8]>) -> Result<bool, String> {
    let environ = match fs::read(format!("/proc/{pid}/environ")) {
        Ok(environ) => environ,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("read Xray accounting environment: {error}")),
    };
    let mut journal = None;
    let mut shared = None;
    for entry in environ.split(|byte| *byte == 0) {
        if let Some(value) = entry.strip_prefix(b"XRAY_BROCADE_ACCOUNTING_FILE=") {
            journal = Some(value);
        } else if let Some(value) = entry.strip_prefix(b"XRAY_BROCADE_ACCOUNTING_SHARED=") {
            shared = Some(value);
        }
    }
    let Some(journal) = journal else {
        return Ok(false);
    };
    let expected = fs::canonicalize(journal_path(state_dir))
        .map_err(|error| format!("resolve accounting journal: {error}"))?;
    Ok(mode.is_none_or(|mode| shared == Some(mode))
        && Path::new(std::ffi::OsStr::from_bytes(journal))
            .canonicalize()
            .is_ok_and(|path| path == expected))
}

pub(crate) fn promote_shared_writer(
    state_dir: &Path,
    old: crate::ProcessRef,
    candidate: crate::ProcessRef,
) -> Result<(), String> {
    if crate::process_is_live(old) {
        return Err("cannot promote accounting while the old writer is alive".to_owned());
    }
    if !crate::process_is_live(candidate) {
        return Err("cannot promote accounting to a dead candidate".to_owned());
    }
    if !process_uses_shared_journal(state_dir, candidate.pid, b"join")? {
        return Err("candidate did not join the managed accounting journal".to_owned());
    }
    promote_journal(&journal_path(state_dir), old, candidate)?;
    let site_path = site_journal_path(state_dir);
    if let Some(site) = header_at(&site_path)? {
        let writer = u32::from_le_bytes(site[28..32].try_into().map_err(|_| "site journal pid")?);
        let ticks = u64::from_le_bytes(site[32..40].try_into().map_err(|_| "site journal ticks")?);
        if (writer == old.pid as u32 && ticks == old.start_ticks)
            || (writer == candidate.pid as u32 && ticks == candidate.start_ticks)
        {
            promote_journal(&site_path, old, candidate)?;
        }
    }
    Ok(())
}

fn promote_journal(
    path: &Path,
    old: crate::ProcessRef,
    candidate: crate::ProcessRef,
) -> Result<(), String> {
    let mut bytes = header_at(path)?.ok_or("managed accounting journal is absent")?;
    let writer = u32::from_le_bytes(bytes[28..32].try_into().map_err(|_| "journal pid")?);
    let ticks = u64::from_le_bytes(bytes[32..40].try_into().map_err(|_| "journal ticks")?);
    if writer == candidate.pid as u32 && ticks == candidate.start_ticks {
        // A crash after the durable PID-last publication but before the upgrade runtime was
        // removed must be recoverable without inventing a second accounting epoch.
        return Ok(());
    }
    if writer != old.pid as u32 || ticks != old.start_ticks {
        return Err("accounting writer changed before promotion".to_owned());
    }
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|error| format!("read boot id: {error}"))?;
    let compact = boot.trim().replace('-', "");
    if compact.len() != 32 {
        return Err("invalid boot id while promoting accounting".to_owned());
    }
    let mut boot_bytes = [0_u8; 16];
    for (index, byte) in boot_bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&compact[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid boot id while promoting accounting")?;
    }
    bytes[32..40].copy_from_slice(&candidate.start_ticks.to_le_bytes());
    bytes[40..56].copy_from_slice(&boot_bytes);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("open accounting journal for promotion: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect accounting journal for promotion: {error}"))?;
    if !metadata.is_file()
        || metadata.len() != JOURNAL_SIZE
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("accounting journal is unsafe for promotion".to_owned());
    }
    file.write_all_at(&bytes[32..56], 32)
        .map_err(|error| format!("write accounting candidate identity: {error}"))?;
    file.sync_data()
        .map_err(|error| format!("sync accounting candidate identity: {error}"))?;
    file.write_all_at(&(candidate.pid as u32).to_le_bytes(), 28)
        .map_err(|error| format!("publish accounting candidate writer: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync accounting writer promotion: {error}"))?;
    let promoted = header_at(path)?.ok_or("accounting journal vanished after promotion")?;
    if u32::from_le_bytes(promoted[28..32].try_into().map_err(|_| "journal pid")?)
        != candidate.pid as u32
        || u64::from_le_bytes(promoted[32..40].try_into().map_err(|_| "journal ticks")?)
            != candidate.start_ticks
    {
        return Err("accounting writer promotion did not persist".to_owned());
    }
    Ok(())
}

pub(crate) fn sync_live_journal(
    state_dir: &Path,
    process_epoch: &str,
    pid: i32,
) -> Result<(), String> {
    if epoch(state_dir, process_epoch, pid)?.starts_with("journal:") {
        fs::File::open(journal_path(state_dir))
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("sync live accounting journal: {error}"))?;
        if header_at(&site_journal_path(state_dir))?.is_some() {
            fs::File::open(site_journal_path(state_dir))
                .and_then(|file| file.sync_all())
                .map_err(|error| format!("sync site accounting journal: {error}"))?;
        }
    }
    Ok(())
}

/// Only read the complete mapping after its writer exited. Live samples use Xray's atomic
/// QueryStats; treating a concurrently changing file as an atomic snapshot would be incorrect.
fn stopped_stats(state_dir: &Path, pid: i32) -> Result<Option<Vec<XrayStat>>, String> {
    let Some(header) = header(state_dir)? else {
        return Ok(None);
    };
    if u32::from_le_bytes(header[28..32].try_into().map_err(|_| "journal pid")?) != pid as u32 {
        return Ok(None);
    }
    let file = fs::File::open(journal_path(state_dir)).map_err(|e| e.to_string())?;
    file.sync_all()
        .map_err(|e| format!("sync accounting journal: {e}"))?;
    let bytes = fs::read(journal_path(state_dir)).map_err(|e| e.to_string())?;
    let mut stats = decode_stats(&bytes)?;
    let site_path = site_journal_path(state_dir);
    if let Some(site_header) = header_at(&site_path)? {
        if u32::from_le_bytes(
            site_header[28..32]
                .try_into()
                .map_err(|_| "site journal pid")?,
        ) != pid as u32
        {
            // A rolled-back binary may not support site observations. Never certify
            // the stale file as its counters, or block ordinary accounting shutdown.
            return Ok(Some(stats));
        }
        fs::File::open(&site_path)
            .and_then(|file| file.sync_all())
            .map_err(|e| format!("sync site journal: {e}"))?;
        stats.extend(decode_stats(
            &fs::read(&site_path).map_err(|e| e.to_string())?,
        )?);
        stats.push(XrayStat {
            name: format!(
                "site>>>epoch:{}",
                site_header[8..24]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
            value: 1,
        });
    }
    Ok(Some(stats))
}

fn decode_stats(bytes: &[u8]) -> Result<Vec<XrayStat>, String> {
    if bytes.len() as u64 != JOURNAL_SIZE || &bytes[..8] != b"BRACCT02" {
        return Err("invalid accounting journal".to_owned());
    }
    let count = u32::from_le_bytes(bytes[24..28].try_into().map_err(|_| "journal count")?) as usize;
    if count > CAPACITY {
        return Err("invalid accounting journal count".to_owned());
    }
    (0..count)
        .map(|index| {
            let record =
                &bytes[HEADER_SIZE + index * RECORD_SIZE..HEADER_SIZE + (index + 1) * RECORD_SIZE];
            let length =
                u32::from_le_bytes(record[8..12].try_into().map_err(|_| "journal label")?) as usize;
            if length == 0 || length > RECORD_SIZE - 16 {
                return Err("invalid accounting journal label".to_owned());
            }
            let name = std::str::from_utf8(&record[16..16 + length])
                .map_err(|_| "invalid journal UTF-8")?
                .to_owned();
            let value = i64::from_le_bytes(record[..8].try_into().map_err(|_| "journal value")?);
            if value < 0 {
                return Err("negative accounting journal counter".to_owned());
            }
            Ok(XrayStat { name, value })
        })
        .collect()
}

/// Bind recovery to the journal namespaces before any process is retired. Owners may change
/// again if the replacement crashes, but the cumulative counter namespaces must never change.
pub(crate) fn handoff_journal_epochs(state_dir: &Path) -> Result<[Option<String>; 2], String> {
    let mut epochs = [None, None];
    for (index, path) in [journal_path(state_dir), site_journal_path(state_dir)]
        .iter()
        .enumerate()
    {
        if let Some(header) = header_at(path)? {
            epochs[index] = Some(
                header[8..24]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            );
        } else if index == 0 {
            return Err("handoff accounting journal is absent".to_owned());
        }
    }
    Ok(epochs)
}

/// Freeze shared counters after every writer exited, before a fresh owner reopens the same files.
/// No zero interval is certified: the candidate's actual crash time is unknown.
pub(crate) fn sync_after_handoff_loss(
    state_dir: &Path,
    epochs: &[Option<String>; 2],
) -> Result<(), String> {
    if !crate::serving_xray_processes_in(state_dir).is_empty() {
        return Err("cannot freeze accounting while a managed Xray is alive".to_owned());
    }
    if &handoff_journal_epochs(state_dir)? != epochs {
        return Err("handoff accounting namespace changed during recovery".to_owned());
    }
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|error| error.to_string())?
        .trim()
        .replace('-', "");
    for path in [journal_path(state_dir), site_journal_path(state_dir)] {
        let Some(header) = header_at(&path)? else {
            continue;
        };
        let pid = u32::from_le_bytes(header[28..32].try_into().map_err(|_| "journal pid")?);
        let ticks = u64::from_le_bytes(header[32..40].try_into().map_err(|_| "journal ticks")?);
        let writer_boot = header[40..56]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if boot == writer_boot
            && crate::process_ref(pid as i32).is_some_and(|process| process.start_ticks == ticks)
        {
            return Err("cannot freeze accounting while its writer is alive".to_owned());
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.len() != JOURNAL_SIZE
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("handoff accounting journal is unsafe".to_owned());
        }
        file.sync_all()
            .map_err(|error| format!("sync crashed handoff journal: {error}"))?;
        let mut bytes = Vec::with_capacity(JOURNAL_SIZE as usize);
        file.take(JOURNAL_SIZE + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read frozen handoff journal: {error}"))?;
        decode_stats(&bytes)?;
    }
    Ok(())
}

fn read_stopped(state_dir: &Path) -> Result<Option<StoppedUsage>, String> {
    match fs::read(state_dir.join(STOP_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("decode stopped usage: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read stopped usage: {e}")),
    }
}

fn write_stopped(state_dir: &Path, stopped: &StoppedUsage) -> Result<(), String> {
    atomic_write_private(
        &state_dir.join(STOP_FILE),
        &serde_json::to_vec(stopped).map_err(|e| e.to_string())?,
    )
}

pub(crate) fn stop(state_dir: &Path) -> Result<(), String> {
    let Some(process) = crate::serving_xray_process_in(state_dir) else {
        return Ok(());
    };
    // A legacy process may have no generation (initial installation). It has no accounted
    // namespace to protect. Every established namespace must be durably sampled before stopping.
    if crate::read_usage_generation(state_dir)?.is_none() {
        return crate::terminate_processes(
            &[process],
            crate::XRAY_TERM_GRACE,
            crate::XRAY_KILL_GRACE,
        );
    }
    let report = crate::read_usage_report(state_dir)?;
    spool_push(USAGE_SPOOL, state_dir, &report)?;
    let mut stopped = StoppedUsage {
        report,
        writer_pid: process.pid,
        stopped_at_unix_millis: None,
    };
    write_stopped(state_dir, &stopped)?;
    crate::terminate_processes(&[process], crate::XRAY_TERM_GRACE, crate::XRAY_KILL_GRACE)?;
    stopped.stopped_at_unix_millis = Some(current_unix_millis()?);
    if let Some(stats) = stopped_stats(state_dir, process.pid)? {
        stopped.report.counters = crate::usage_counters_from_stats(&stats);
        stopped.report.site_usage = crate::site_usage::observe(&stats);
    }
    stopped.report.read_at_unix_secs = crate::current_unix_secs()?;
    let (instance, sequence) = crate::reserve_usage_sequence(state_dir)?;
    stopped.report.agent_instance_id = instance;
    stopped.report.sequence = sequence;
    write_stopped(state_dir, &stopped)?;
    spool_push(USAGE_SPOOL, state_dir, &stopped.report)
}

pub(crate) fn zero_windows(state_dir: &Path) -> Result<Vec<UsageZeroWindow>, String> {
    match fs::read(state_dir.join(ZERO_FILE)) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("decode zero windows: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("read zero windows: {e}")),
    }
}

/// End certified downtime just before launching: traffic can begin before the API health gate.
pub(crate) fn before_launch(state_dir: &Path) -> Result<(), String> {
    let Some(stopped) = read_stopped(state_dir)? else {
        return Ok(());
    };
    let Some(start) = stopped.stopped_at_unix_millis else {
        return Ok(());
    };
    let end = current_unix_millis()?;
    if end <= start {
        return Ok(());
    }
    // A machine may stay disabled beyond receipt retention. Use a fresh id/time instead of
    // replaying an ancient sequence behind the durable cursor; unchanged cumulative values
    // still contribute zero, and an undelivered tail can only be booked once.
    let mut final_report = stopped.report.clone();
    let (instance, sequence) = crate::reserve_usage_sequence(state_dir)?;
    final_report.agent_instance_id = instance;
    final_report.sequence = sequence;
    final_report.read_at_unix_secs = end / 1000;
    final_report.zero_windows = zero_windows(state_dir)?;
    final_report.zero_windows.push(UsageZeroWindow {
        start_unix_millis: start,
        end_unix_millis: end,
    });
    spool_push(USAGE_SPOOL, state_dir, &final_report)?;
    if !stopped.report.xray_epoch.starts_with("journal:") && journal_path(state_dir).exists() {
        // A legacy rollback breaks counter continuity. Reusing an older journal afterwards
        // would bill its already-accounted totals again; retain one bounded recovery copy.
        fs::rename(
            journal_path(state_dir),
            state_dir.join("xray-accounting.previous.bin"),
        )
        .map_err(|error| format!("archive discontinuous accounting journal: {error}"))?;
    }
    let mut windows = zero_windows(state_dir)?;
    if windows.len() >= 32 {
        return Err("unreported managed downtime queue is full".to_owned());
    }
    windows.push(UsageZeroWindow {
        start_unix_millis: start,
        end_unix_millis: end,
    });
    atomic_write_private(
        &state_dir.join(ZERO_FILE),
        &serde_json::to_vec(&windows).map_err(|e| e.to_string())?,
    )
}

pub(crate) fn launched(state_dir: &Path) -> Result<(), String> {
    if let Some(stopped) = read_stopped(state_dir)? {
        if let (Some(start), Some(header)) = (stopped.stopped_at_unix_millis, header(state_dir)?) {
            let end = i64::from_le_bytes(
                header[56..64]
                    .try_into()
                    .map_err(|_| "journal launch time")?,
            );
            let writer =
                u32::from_le_bytes(header[28..32].try_into().map_err(|_| "journal writer")?);
            if end > start
                && crate::serving_xray_process_in(state_dir)
                    .is_some_and(|process| process.pid as u32 == writer)
            {
                let mut windows = zero_windows(state_dir)?;
                if let Some(window) = windows
                    .iter_mut()
                    .rev()
                    .find(|window| window.start_unix_millis == start)
                {
                    window.end_unix_millis = window.end_unix_millis.max(end);
                } else {
                    if windows.len() >= 32 {
                        return Err("unreported managed downtime queue is full".to_owned());
                    }
                    windows.push(UsageZeroWindow {
                        start_unix_millis: start,
                        end_unix_millis: end,
                    });
                }
                atomic_write_private(
                    &state_dir.join(ZERO_FILE),
                    &serde_json::to_vec(&windows).map_err(|e| e.to_string())?,
                )?;
            }
        }
    }
    remove_if_present(&state_dir.join(STOP_FILE))
}

pub(crate) fn collected(state_dir: &Path) -> Result<(), String> {
    remove_if_present(&state_dir.join(ZERO_FILE))
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove accounting checkpoint: {e}")),
    }
}

pub(crate) fn has_stopped(state_dir: &Path) -> bool {
    state_dir.join(STOP_FILE).exists()
}

pub(crate) fn confirmed_stopped_counters(
    state_dir: &Path,
) -> Result<Option<Vec<brocade_deployment::protocol::UsageCounter>>, String> {
    let Some(stopped) = read_stopped(state_dir)? else {
        return Ok(None);
    };
    Ok(stopped
        .stopped_at_unix_millis
        .map(|_| stopped.report.counters))
}

pub(crate) fn stopped_report(state_dir: &Path) -> Result<UsageReportRequest, String> {
    let stopped = read_stopped(state_dir)?.ok_or("xray is absent without a confirmed stop")?;
    let start = stopped
        .stopped_at_unix_millis
        .ok_or("xray stop boundary is unknown")?;
    let mut report = stopped.report;
    let now = current_unix_millis()?;
    report.read_at_unix_secs = now / 1000;
    let (instance, sequence) = crate::reserve_usage_sequence(state_dir)?;
    report.agent_instance_id = instance;
    report.sequence = sequence;
    report.zero_windows = zero_windows(state_dir)?;
    if now > start {
        report.zero_windows.push(UsageZeroWindow {
            start_unix_millis: start,
            end_unix_millis: now,
        });
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_counter_identity_waits_for_writer_promotion_after_owner_exit() {
        use std::process::Command;

        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = std::env::temp_dir().join(format!(
            "brocade-accounting-promotion-{}",
            std::process::id()
        ));
        crate::create_private_dir(&root).unwrap();
        let mut old = ChildGuard(Command::new("sleep").arg("30").spawn().unwrap());
        let mut candidate = ChildGuard(
            Command::new("sleep")
                .arg("30")
                .env("XRAY_BROCADE_ACCOUNTING_FILE", journal_path(&root))
                .env("XRAY_BROCADE_ACCOUNTING_SHARED", "join")
                .spawn()
                .unwrap(),
        );
        let old_ref = crate::process_ref(old.0.id() as i32).unwrap();
        let candidate_ref = crate::process_ref(candidate.0.id() as i32).unwrap();
        let mut bytes = vec![0_u8; JOURNAL_SIZE as usize];
        bytes[..8].copy_from_slice(b"BRACCT02");
        bytes[8..24].fill(7);
        bytes[28..32].copy_from_slice(&(old_ref.pid as u32).to_le_bytes());
        bytes[32..40].copy_from_slice(&old_ref.start_ticks.to_le_bytes());
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
            .replace('-', "");
        for index in 0..16 {
            bytes[40 + index] = u8::from_str_radix(&boot[index * 2..index * 2 + 2], 16).unwrap();
        }
        atomic_write_private(&journal_path(&root), &bytes).unwrap();
        let process_epoch = format!("boot:{}", candidate_ref.start_ticks);
        let original = epoch(&root, &process_epoch, candidate_ref.pid).unwrap();
        assert!(original.starts_with("journal:"));
        old.0.kill().unwrap();
        old.0.wait().unwrap();
        assert!(epoch(&root, &process_epoch, candidate_ref.pid)
            .unwrap_err()
            .contains("unconfirmed"));
        promote_shared_writer(&root, old_ref, candidate_ref).unwrap();
        assert_eq!(
            epoch(&root, &process_epoch, candidate_ref.pid).unwrap(),
            original
        );
        // A legacy process that never opened this journal keeps its independent identity.
        assert_eq!(
            epoch(&root, "legacy:1", std::process::id() as i32).unwrap(),
            "legacy:1"
        );
        fs::remove_file(journal_path(&root)).unwrap();
        assert!(epoch(&root, &process_epoch, candidate_ref.pid).is_err());
        assert_eq!(
            epoch(&root, "legacy:1", std::process::id() as i32).unwrap(),
            "legacy:1"
        );
        candidate.0.kill().unwrap();
        candidate.0.wait().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_freeze_preserves_billing_and_site_files_and_rejects_changed_epochs() {
        let root =
            std::env::temp_dir().join(format!("brocade-crash-freeze-{}", std::process::id()));
        crate::create_private_dir(&root).unwrap();
        let mut bytes = vec![0_u8; JOURNAL_SIZE as usize];
        bytes[..8].copy_from_slice(b"BRACCT02");
        bytes[8..24].fill(1);
        bytes[24..28].copy_from_slice(&1_u32.to_le_bytes());
        let name = b"user>>>fixture#entry>>>traffic>>>uplink";
        bytes[64..72].copy_from_slice(&12345_i64.to_le_bytes());
        bytes[72..76].copy_from_slice(&(name.len() as u32).to_le_bytes());
        bytes[80..80 + name.len()].copy_from_slice(name);
        for path in [journal_path(&root), site_journal_path(&root)] {
            atomic_write_private(&path, &bytes).unwrap();
        }
        let epochs = handoff_journal_epochs(&root).unwrap();
        sync_after_handoff_loss(&root, &epochs).unwrap();
        assert_eq!(fs::read(journal_path(&root)).unwrap(), bytes);
        assert_eq!(fs::read(site_journal_path(&root)).unwrap(), bytes);
        let mut changed = bytes.clone();
        changed[8] = 2;
        atomic_write_private(&journal_path(&root), &changed).unwrap();
        assert!(sync_after_handoff_loss(&root, &epochs).is_err());
        changed = bytes.clone();
        changed[24..28].copy_from_slice(&(CAPACITY as u32 + 1).to_le_bytes());
        atomic_write_private(&journal_path(&root), &changed).unwrap();
        assert!(sync_after_handoff_loss(&root, &epochs).is_err());
        let process = crate::process_ref(std::process::id() as i32).unwrap();
        changed = bytes;
        changed[28..32].copy_from_slice(&(process.pid as u32).to_le_bytes());
        changed[32..40].copy_from_slice(&process.start_ticks.to_le_bytes());
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
            .replace('-', "");
        for index in 0..16 {
            changed[40 + index] = u8::from_str_radix(&boot[index * 2..index * 2 + 2], 16).unwrap();
        }
        atomic_write_private(&journal_path(&root), &changed).unwrap();
        assert!(sync_after_handoff_loss(&root, &epochs)
            .unwrap_err()
            .contains("writer is alive"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn journal_decode_is_bounded_and_preserves_full_counters() {
        let mut bytes = vec![0; JOURNAL_SIZE as usize];
        bytes[..8].copy_from_slice(b"BRACCT02");
        bytes[24..28].copy_from_slice(&1_u32.to_le_bytes());
        let name = b"user>>>alice#entry>>>traffic>>>downlink";
        bytes[64..72].copy_from_slice(&12345_i64.to_le_bytes());
        bytes[72..76].copy_from_slice(&(name.len() as u32).to_le_bytes());
        bytes[80..80 + name.len()].copy_from_slice(name);
        let stats = decode_stats(&bytes).unwrap();
        assert_eq!(stats[0].value, 12345);
        assert_eq!(stats[0].name, std::str::from_utf8(name).unwrap());
        bytes[24..28].copy_from_slice(&(CAPACITY as u32 + 1).to_le_bytes());
        assert!(decode_stats(&bytes).is_err());
    }

    #[test]
    fn confirmed_stop_reports_unchanged_counters_not_a_counter_reset() {
        let dir =
            std::env::temp_dir().join(format!("brocade-accounting-stop-{}", std::process::id()));
        crate::create_private_dir(&dir).unwrap();
        let now = current_unix_millis().unwrap();
        let report = UsageReportRequest {
            agent_instance_id: "fixture".to_owned(),
            sequence: 1,
            usage_generation_id: 9,
            read_at_unix_secs: now / 1000 - 10,
            xray_started_at_unix_secs: now / 1000 - 100,
            xray_epoch: "journal:fixture".to_owned(),
            route: None,
            counters: vec![brocade_deployment::protocol::UsageCounter {
                label: "alice#entry".to_owned(),
                uplink_bytes: 123,
                downlink_bytes: 456,
            }],
            zero_windows: Vec::new(),
            site_usage: None,
        };
        write_stopped(
            &dir,
            &StoppedUsage {
                report: report.clone(),
                writer_pid: 1,
                stopped_at_unix_millis: None,
            },
        )
        .unwrap();
        assert!(
            stopped_report(&dir).is_err(),
            "uncertain exit must never become zero"
        );
        assert!(confirmed_stopped_counters(&dir).unwrap().is_none());
        write_stopped(
            &dir,
            &StoppedUsage {
                report: report.clone(),
                writer_pid: 1,
                stopped_at_unix_millis: Some(now - 5000),
            },
        )
        .unwrap();
        let zero = stopped_report(&dir).unwrap();
        assert_eq!(zero.counters, report.counters);
        assert_eq!(zero.xray_epoch, report.xray_epoch);
        assert_eq!(zero.zero_windows[0].start_unix_millis, now - 5000);
        assert!(zero.zero_windows[0].end_unix_millis >= now);
        before_launch(&dir).unwrap();
        assert_eq!(zero_windows(&dir).unwrap().len(), 1);
        launched(&dir).unwrap();
        assert!(!has_stopped(&dir));
        assert_eq!(
            zero_windows(&dir).unwrap().len(),
            1,
            "startup does not acknowledge undelivered zero coverage"
        );
        collected(&dir).unwrap();
        assert!(zero_windows(&dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY pointing to a newly built managed Xray"]
    fn native_xray_restarts_keep_unsampled_tail_and_counter_identity() {
        use std::{
            io::{Read, Write},
            net::{TcpListener, TcpStream},
            process::{Command, Stdio},
            thread,
            time::{Duration, Instant},
        };
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let binary = std::env::var("BROCADE_TEST_XRAY").expect("newly built Xray is required");
        let old_binary = std::env::var("BROCADE_TEST_XRAY_OLD").unwrap_or_else(|_| binary.clone());
        let launches = std::cell::Cell::new(0);
        let dir =
            std::env::temp_dir().join(format!("brocade-native-accounting-{}", std::process::id()));
        crate::create_private_dir(&dir).unwrap();
        let port = || {
            TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        };
        let api_address = std::net::SocketAddr::from(([127, 0, 0, 1], port()));
        let inbound_port = port();
        let addresses = crate::run_command("ip", &["-o", "-4", "addr", "show"]).unwrap();
        let target_host = addresses
            .split_whitespace()
            .filter_map(|value| {
                value
                    .split_once('/')
                    .and_then(|(ip, _)| ip.parse::<std::net::IpAddr>().ok())
            })
            .find(|ip| !brocade_core::forwarding::target_ip_is_blocked_by_policy(*ip, true))
            .expect("accounting fixture needs a permitted local IPv4 address");
        let listener = TcpListener::bind((target_host, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let target_port = listener.local_addr().unwrap().port();
        let blocked_cidrs =
            brocade_core::forwarding::blocked_target_cidrs(true).collect::<Vec<_>>();
        let config = dir.join("xray.json");
        fs::write(&config, serde_json::to_vec(&serde_json::json!({
            "log": {"loglevel": "warning"}, "stats": {},
            "api": {"tag": "api", "services": ["StatsService"]},
            "policy": {"system": {"statsInboundUplink": true, "statsInboundDownlink": true}},
            "inbounds": [
                {"tag": "api", "listen": "127.0.0.1", "port": api_address.port(), "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1"}},
                {"tag": "fwd:7", "listen": "127.0.0.1", "port": inbound_port, "protocol": "dokodemo-door", "settings": {"address": target_host.to_string(), "port": target_port, "network": "tcp", "forwardingTargetPolicy": {"blocked_cidrs": blocked_cidrs}}}
            ], "outbounds": [{"protocol": "freedom", "tag": "out:forward", "settings": {"ipsBlocked": []}}],
            "routing": {"rules": [{"type": "field", "inboundTag": ["api"], "outboundTag": "api"}]}
        })).unwrap()).unwrap();
        let launch = || {
            let executable = if launches.get() == 0 {
                &old_binary
            } else {
                &binary
            };
            launches.set(launches.get() + 1);
            let log = fs::File::create(dir.join(format!("xray-{}.log", launches.get()))).unwrap();
            let mut child = ChildGuard(
                Command::new(executable)
                    .args(["run", "-config", config.to_str().unwrap()])
                    .env("XRAY_BROCADE_ACCOUNTING_FILE", journal_path(&dir))
                    .stdout(Stdio::from(log.try_clone().unwrap()))
                    .stderr(Stdio::from(log))
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if crate::xray_grpc::query_stats(api_address, "inbound>>>fwd:7").is_ok() {
                    break;
                }
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "Xray exited before serving"
                );
                assert!(Instant::now() < deadline, "Xray did not serve in time");
                thread::sleep(Duration::from_millis(10));
            }
            child
        };
        let mut old = launch();
        let mut client = TcpStream::connect(("127.0.0.1", inbound_port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        client.write_all(&[1; 1024]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut server = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "accounting target was not reached: {}",
                        fs::read_to_string(dir.join("xray-1.log")).unwrap()
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accounting fixture accept: {error}"),
            }
        };
        server
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        server.read_exact(&mut [0; 1024]).unwrap();
        server.write_all(&[2; 2048]).unwrap();
        client.read_exact(&mut [0; 2048]).unwrap();
        let baseline = crate::xray_grpc::query_stats(api_address, "inbound>>>fwd:7").unwrap();
        assert_eq!(baseline.iter().map(|stat| stat.value).sum::<i64>(), 3072);
        let original_header = header(&dir).unwrap().unwrap();
        let original_ticks = u64::from_le_bytes(original_header[32..40].try_into().unwrap());
        let original_epoch =
            epoch(&dir, &format!("1234:{original_ticks}"), old.0.id() as i32).unwrap();
        assert!(
            original_epoch.starts_with("journal:"),
            "journal identity must not depend on NTP-adjustable btime"
        );
        // These bytes occur after the last QueryStats. No final API sample precedes shutdown.
        client.write_all(&[3; 512]).unwrap();
        server.read_exact(&mut [0; 512]).unwrap();
        server.write_all(&[4; 1024]).unwrap();
        client.read_exact(&mut [0; 1024]).unwrap();
        thread::sleep(Duration::from_millis(20));
        let process = crate::process_ref(old.0.id() as i32).unwrap();
        crate::terminate_processes(&[process], crate::XRAY_TERM_GRACE, crate::XRAY_KILL_GRACE)
            .unwrap();
        old.0.wait().unwrap();
        let final_stats = stopped_stats(&dir, process.pid).unwrap().unwrap();
        let counters = final_stats
            .iter()
            .filter(|stat| stat.name.starts_with("inbound>>>fwd:7"))
            .collect::<Vec<_>>();
        assert_eq!(
            counters.iter().map(|stat| stat.value).sum::<i64>(),
            4608,
            "the unsampled tail must survive"
        );
        let mut new = launch();
        let restored = crate::xray_grpc::query_stats(api_address, "inbound>>>fwd:7").unwrap();
        assert_eq!(
            restored.iter().map(|stat| stat.value).sum::<i64>(),
            4608,
            "startup must not zero cumulative counters"
        );
        let restored_header = header(&dir).unwrap().unwrap();
        let restored_ticks = u64::from_le_bytes(restored_header[32..40].try_into().unwrap());
        assert_eq!(
            epoch(&dir, &format!("5678:{restored_ticks}"), new.0.id() as i32).unwrap(),
            original_epoch
        );
        assert_eq!(
            original_header[8..24],
            restored_header[8..24],
            "counter identity stays stable"
        );
        assert_ne!(
            original_header[28..40],
            restored_header[28..40],
            "process identity changes"
        );
        new.0.kill().unwrap();
        new.0.wait().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
