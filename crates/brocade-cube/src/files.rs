use crate::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path},
};

pub(crate) fn clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

pub(crate) fn private_dir(path: &Path) -> Result<()> {
    if !clean_absolute(path) {
        return Err("state directory must be an absolute clean path".into());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|_| "cannot create private Cube state directory")?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "Cube state directory unavailable")?;
    // SAFETY: geteuid has no pointer arguments or preconditions.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err("Cube state directory must be private and owned by the Agent UID".into());
    }
    Ok(())
}

pub(crate) fn checked_file(path: &Path, limit: usize, private: bool) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "Cube private file is unavailable")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cube file metadata unavailable")?;
    // SAFETY: geteuid has no pointer arguments or preconditions.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
        || (private && metadata.mode() & 0o077 != 0)
        || metadata.len() > limit as u64
    {
        return Err("Cube file ownership, mode or size is unsafe".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cube private file read failed")?;
    if bytes.len() > limit {
        return Err("Cube private file exceeds limit".into());
    }
    Ok(bytes)
}

pub(crate) fn nonce() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| "Cube execution entropy unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub(crate) fn atomic_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| "Cube execution record serialization failed")?;
    if bytes.len() > 32 * 1024 {
        return Err("Cube execution record exceeds limit".into());
    }
    let parent = path
        .parent()
        .ok_or("Cube execution record has no directory")?;
    let temporary = parent.join(format!(".cvm-{}", nonce()?));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|_| "cannot create Cube execution checkpoint")?;
        file.write_all(&bytes)
            .map_err(|_| "cannot write Cube execution checkpoint")?;
        file.sync_all()
            .map_err(|_| "cannot sync Cube execution checkpoint")?;
        drop(file);
        fs::rename(&temporary, path).map_err(|_| "cannot replace Cube execution checkpoint")?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| "cannot sync Cube execution directory")
    })();
    if temporary.exists() {
        fs::remove_file(&temporary)
            .map_err(|_| "cannot remove failed Cube execution checkpoint")?;
    }
    result.map_err(str::to_owned)
}

pub(crate) fn lock_directory(root: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join(".execution.lock"))
        .map_err(|_| "Cube execution lock unavailable")?;
    use std::os::fd::AsRawFd;
    // SAFETY: file owns this live descriptor; flock does not retain a pointer.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("another Agent owns the Cube execution directory".into());
    }
    Ok(file)
}

use std::os::unix::fs::DirBuilderExt;
