//! Small durable-file primitives shared by artifacts and retry spools.
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Replace a private file without ever exposing a truncated or partially-written
/// destination.  The temporary file lives beside the destination, making rename
/// atomic on the filesystems on which the state directory is supported.
pub(crate) fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<(), String> {
    atomic_write_with_mode(path, contents, 0o600)
}

/// Atomically replace a file and make its intended Unix mode part of the
/// replacement, rather than fixing permissions after the rename. This is used
/// for root-owned operating-system configuration that must stay readable
/// across boot even if the process is interrupted immediately after rename.
pub(crate) fn atomic_write_with_mode(
    path: &Path,
    contents: &[u8],
    mode: u32,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string_lossy();
    let (temporary, mut file) = (0..128)
        .find_map(|_| {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temporary = parent.join(format!(".{name}.tmp.{}.{}", std::process::id(), sequence));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&temporary)
            {
                Ok(file) => Some(Ok((temporary, file))),
                // A killed process may leave its temporary file behind, and a
                // later process may eventually reuse the same pid. Skip stale
                // names instead of turning crash recovery into a write outage.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(format!("open {}: {error}", temporary.display()))),
            }
        })
        .unwrap_or_else(|| {
            Err(format!(
                "cannot allocate a temporary file beside {}",
                path.display()
            ))
        })?;

    let result = (|| {
        // OpenOptionsExt::mode is still filtered through the process umask.
        // Correct the unpublished temporary inode before it becomes visible at
        // the destination name.
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|error| format!("chmod {}: {error}", temporary.display()))?;
        file.write_all(contents)
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("fsync {}: {error}", temporary.display()))?;
        fs::rename(&temporary, path).map_err(|error| {
            format!(
                "rename {} to {}: {error}",
                temporary.display(),
                path.display()
            )
        })?;
        // fsyncing only the file does not make the directory entry durable.  A
        // power loss after rename could otherwise resurrect the old queue.
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("fsync directory {}: {error}", parent.display()))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::{env, fs, os::unix::fs::PermissionsExt};

    use super::{atomic_write_private, atomic_write_with_mode};

    #[test]
    fn replacement_is_private_and_leaves_no_temporary_file() {
        let dir = env::temp_dir().join(format!("brocade-atomic-write-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        atomic_write_private(&path, b"old").unwrap();
        atomic_write_private(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn replacement_can_be_world_readable_at_rename_time() {
        let dir = env::temp_dir().join(format!("brocade-atomic-mode-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("module.conf");

        atomic_write_with_mode(&path, b"tls\n", 0o644).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"tls\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let _ = fs::remove_dir_all(dir);
    }
}
