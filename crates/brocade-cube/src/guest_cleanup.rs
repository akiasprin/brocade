//! Durable, bounded cleanup of this executor's exact Guest exec identities.
use crate::{
    backend::{valid_runtime, Inner},
    files,
    guest::remaining,
    Result,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub(crate) const MAX_IO: usize = 64;

#[derive(Deserialize, Serialize)]
pub(crate) struct Intent {
    pub(crate) runtime: String,
    pub(crate) exec_id: String,
}

pub(crate) fn valid_exec(id: &str) -> bool {
    id.strip_prefix("brocade-terminal-")
        .or_else(|| id.strip_prefix("brocade-"))
        .is_some_and(|nonce| nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub(crate) fn cleanup_exec(
    inner: &Inner,
    runtime: &str,
    id: &str,
    deadline: Instant,
) -> Result<()> {
    if !valid_runtime(runtime) || !valid_exec(id) {
        return Err("refusing to clean an unproven Guest exec identity".into());
    }
    let request = json!({"container_id":runtime,"exec_id":id});
    let mut close = request.clone();
    close["stdin"] = json!(true);
    let mut kill = request.clone();
    kill["signal"] = json!(libc::SIGKILL);
    // Guest stdin can already be closed after Wait. CloseIO is only a hint;
    // exact-exec Kill and DeleteProcess acknowledgements prove cleanup below.
    let _ = inner.task("CloseIO", close, remaining(deadline)?);
    let mut failure = None;
    for (method, payload) in [("Kill", kill), ("DeleteProcess", request)] {
        let result = inner.task(method, payload, remaining(deadline)?);
        if let Err(error) = result {
            if !error.contains("NotFound") {
                failure = Some(error);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

impl Inner {
    pub(crate) fn recover_guest_io(&self) -> Result<()> {
        let Ok(_recovery) = self.guest_recovery.try_lock() else {
            return Ok(());
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut visited = 0;
        for entry in
            std::fs::read_dir(&self.root).map_err(|_| "Guest cleanup directory unavailable")?
        {
            let entry = entry.map_err(|_| "Guest cleanup entry unavailable")?;
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".guest-io-")
            {
                continue;
            }
            visited += 1;
            if visited > MAX_IO {
                return Err(
                    "Guest execution history limit reached; refusing unbounded cleanup".into(),
                );
            }
            let directory = entry.path();
            if self
                .guest_io
                .lock()
                .map_err(|_| "active Guest execution state unavailable")?
                .contains(&directory)
            {
                continue;
            }
            let checkpoint = directory.join("exec.json");
            // A crash can precede recording the exec identity. Without ownership
            // proof no Guest process may be signalled.
            if !checkpoint
                .try_exists()
                .map_err(|_| "Guest cleanup checkpoint unavailable")?
            {
                continue;
            }
            verify_directory(&directory)?;
            let intent: Intent =
                serde_json::from_slice(&files::checked_file(&checkpoint, 4096, true)?)
                    .map_err(|_| "invalid durable Guest cleanup identity")?;
            if !self
                .state()?
                .records
                .values()
                .any(|r| r.runtime == intent.runtime)
            {
                continue;
            }
            cleanup_exec(self, &intent.runtime, &intent.exec_id, deadline)?;
            std::fs::remove_dir_all(&directory)
                .map_err(|_| "Guest cleanup checkpoint removal failed")?;
        }
        Ok(())
    }
}

fn verify_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let info =
        std::fs::symlink_metadata(path).map_err(|_| "Guest cleanup directory unavailable")?;
    // SAFETY: geteuid has no pointer arguments or preconditions.
    let uid = unsafe { libc::geteuid() };
    if !info.is_dir() || info.uid() != uid || info.mode() & 0o077 != 0 {
        return Err("Guest cleanup directory ownership or mode is unsafe".into());
    }
    Ok(())
}
