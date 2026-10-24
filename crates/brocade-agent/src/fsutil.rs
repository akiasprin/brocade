//! Small durable-file primitives shared by artifacts and retry spools.
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Read a bounded regular checkpoint without blocking on a FIFO or following a leaf symlink.
pub(crate) fn read_bounded(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, String> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular checkpoint", path.display()));
    }
    if metadata.len() > maximum as u64 {
        return Err(format!("{} exceeds {maximum} bytes", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.len() > maximum {
        return Err(format!("{} exceeds {maximum} bytes", path.display()));
    }
    Ok(Some(bytes))
}

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

    use super::{atomic_write_private, atomic_write_with_mode, read_bounded};

    #[test]
    fn bounded_checkpoint_read_rejects_nonregular_files_and_symlinks() {
        let dir = env::temp_dir().join(format!("brocade-bounded-files-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        assert!(read_bounded(&path, 4).unwrap().is_none());
        atomic_write_private(&path, b"test").unwrap();
        assert_eq!(read_bounded(&path, 4).unwrap().unwrap(), b"test");
        assert!(read_bounded(&path, 3).unwrap_err().contains("exceeds"));
        let link = dir.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_bounded(&link, 4).is_err());
        let fifo = dir.join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: name is a NUL-terminated temporary pathname and the mode is valid.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_bounded(&fifo, 4)
            .unwrap_err()
            .contains("not a regular"));
        assert_eq!(fs::read(&path).unwrap(), b"test");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bounded_checkpoint_read_rejects_sparse_files_under_memory_limit() {
        if env::var_os("BROCADE_BOUNDED_READ_CHILD").is_none() {
            let result = std::process::Command::new(env::current_exe().unwrap())
                .args(["--exact", "fsutil::tests::bounded_checkpoint_read_rejects_sparse_files_under_memory_limit", "--test-threads=1"])
                .env("BROCADE_BOUNDED_READ_CHILD", "1")
                .output().unwrap();
            assert!(
                result.status.success(),
                "bounded-read child failed: {}",
                String::from_utf8_lossy(&result.stdout)
            );
            return;
        }
        let limits = libc::rlimit {
            rlim_cur: 512 * 1024 * 1024,
            rlim_max: 512 * 1024 * 1024,
        };
        // SAFETY: only this isolated test subprocess receives the address-space limit.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_AS, &limits) }, 0);
        let dir = env::temp_dir().join(format!("brocade-bounded-sparse-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        fs::File::create(&path)
            .unwrap()
            .set_len(2 * 1024 * 1024 * 1024)
            .unwrap();
        assert!(read_bounded(&path, 128 * 1024)
            .unwrap_err()
            .contains("exceeds 131072"));
        fs::remove_dir_all(dir).unwrap();
    }

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
