// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
//! The volume-only mode shares the local engine's namespace and queries. It
//! never copies a system image: Cubelet formats an empty writable OverlayFS disk.
use std::collections::HashMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use super::reflink::NameKind;
use crate::pkg::errors::{CubecowError, CubecowResult};

fn bad(message: &str) -> CubecowError {
    CubecowError::PreconditionFailed(message.into())
}
fn sync(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}
fn uid() -> u32 {
    // SAFETY: geteuid has no arguments or preconditions.
    unsafe { libc::geteuid() }
}
fn private_dir(path: &Path) -> CubecowResult<()> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current);
                current = current
                    .parent()
                    .ok_or_else(|| bad("plain storage parent is missing"))?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    check_dir(path)?;
    // fsync each newly created entry's parent, including a new pool's ancestors.
    for directory in missing.into_iter().rev() {
        sync(directory)?;
        if let Some(parent) = directory.parent() {
            sync(parent)?;
        }
    }
    Ok(())
}
fn check_dir(path: &Path) -> CubecowResult<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.uid() != uid() || m.mode() & 0o077 != 0 {
        return Err(bad("plain storage directory is not private and owned"));
    }
    Ok(())
}
fn owned_file(path: &Path) -> CubecowResult<fs::Metadata> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.uid() != uid() || m.mode() & 0o077 != 0 || m.nlink() != 1 {
        return Err(bad("plain storage file identity or permissions are unsafe"));
    }
    Ok(m)
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

fn remove_unpublished(dir: &Path, name: Option<&str>) -> CubecowResult<()> {
    check_dir(dir)?;
    for (count, entry) in fs::read_dir(dir)?.enumerate() {
        if count >= if name.is_some() { 3 } else { 1 } {
            return Err(bad("unpublished plain volume has too many objects"));
        }
        let entry = entry?;
        let file = entry.file_name();
        let allowed = match name {
            Some(name) => file == name || file == ".initialized" || file == ".initialized.pending",
            None => safe_name(&file.to_string_lossy()),
        };
        if !allowed {
            return Err(bad("unpublished plain volume contains an unknown object"));
        }
        owned_file(&entry.path())?;
    }
    for entry in fs::read_dir(dir)? {
        fs::remove_file(entry?.path())?;
    }
    sync(dir)?;
    fs::remove_dir(dir)?;
    Ok(())
}

pub(super) fn initialize(root: &Path) -> CubecowResult<(File, HashMap<String, NameKind>)> {
    private_dir(root)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join(".plain.lock"))?;
    owned_file(&root.join(".plain.lock"))?;
    // SAFETY: the descriptor is live and flock retains its lock until File drops.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(bad("plain storage is already owned by another process"));
    }
    let volumes = root.join("volumes");
    private_dir(&volumes)?;
    let marker = root.join(".plain-v1");
    if !marker.exists() {
        if fs::read_dir(&volumes)?.next().is_some() {
            return Err(bad("plain storage refuses an unregistered existing pool"));
        }
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&marker)?;
        f.write_all(b"brocade-plain-v1\n")?;
        f.sync_all()?;
        sync(root)?;
    }
    owned_file(&marker)?;
    if fs::metadata(&marker)?.len() != 17 || fs::read(&marker)? != b"brocade-plain-v1\n" {
        return Err(bad("plain storage pool identity is invalid"));
    }
    let pending = root.join(".pending");
    private_dir(&pending)?;
    // Only unpublished, private allocation directories are reclaimable. A
    // committed volume (even an uninitialised one) never enters this cleanup.
    for (count, entry) in fs::read_dir(&pending)?.enumerate() {
        if count >= 8192 {
            return Err(bad("too many pending plain allocations"));
        }
        let entry = entry?;
        if uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err() {
            return Err(bad("unknown plain allocation staging entry"));
        }
        remove_unpublished(&entry.path(), None)?;
    }
    sync(&pending)?;
    let trash = root.join(".trash");
    private_dir(&trash)?;
    for (count, entry) in fs::read_dir(&trash)?.enumerate() {
        if count >= 8192 {
            return Err(bad("too many pending plain deletions"));
        }
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !safe_name(&name) {
            return Err(bad("unknown plain volume deletion entry"));
        }
        remove_unpublished(&entry.path(), Some(&name))?;
    }
    sync(&trash)?;
    let mut index = HashMap::new();
    for entry in fs::read_dir(&volumes)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !safe_name(&name) || index.len() >= 8192 {
            return Err(bad("invalid or oversized plain volume namespace"));
        }
        check_dir(&entry.path())?;
        for file in fs::read_dir(entry.path())? {
            let file = file?;
            if file.file_name() != name.as_str()
                && file.file_name() != ".initialized"
                && file.file_name() != ".initialized.pending"
            {
                return Err(bad("plain pool contains an unsupported object"));
            }
            owned_file(&file.path())?;
        }
        // Keep a known binding even when its disk is missing or sparse. Queries
        // quarantine that instance without hiding it or stopping healthy peers.
        index.insert(name, NameKind::Volume);
    }
    Ok((lock, index))
}

pub(super) fn volume_metadata(path: &Path) -> CubecowResult<fs::Metadata> {
    check_dir(
        path.parent()
            .ok_or_else(|| bad("plain volume directory is missing"))?,
    )?;
    let m = owned_file(path)?;
    if m.len() == 0 || m.blocks().saturating_mul(512) < m.len() {
        return Err(bad("plain volume is missing its physical allocation"));
    }
    Ok(m)
}

pub(super) fn create(volumes: &Path, name: &str, size: u64, reserve: u64) -> CubecowResult<()> {
    if !safe_name(name) || size == 0 || size > i64::MAX as u64 {
        return Err(CubecowError::InvalidArg(
            "invalid plain volume name or size".into(),
        ));
    }
    let root = volumes
        .parent()
        .ok_or_else(|| bad("plain storage root is missing"))?;
    let staging = root.join(".pending").join(uuid::Uuid::new_v4().to_string());
    private_dir(&staging)?;
    let result = (|| -> CubecowResult<()> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(staging.join(name))?;
        // SAFETY: fstatvfs writes a valid statvfs into the supplied live pointer.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(f.as_raw_fd(), &mut stat) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let free = (stat.f_bavail as u128) * (stat.f_frsize as u128);
        if free < u128::from(size) + u128::from(reserve) {
            return Err(bad(
                "plain volume would consume the filesystem safety reserve",
            ));
        }
        // SAFETY: size is checked to fit off_t; the descriptor is writable.
        if unsafe { libc::fallocate(f.as_raw_fd(), 0, 0, size as libc::off_t) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        f.sync_all()?;
        sync(&staging)?;
        let from = std::ffi::CString::new(staging.as_os_str().as_encoded_bytes())
            .map_err(|_| bad("invalid staging path"))?;
        let to = std::ffi::CString::new(volumes.join(name).as_os_str().as_encoded_bytes())
            .map_err(|_| bad("invalid volume path"))?;
        // SAFETY: both C strings are live and terminated for this Linux syscall.
        // musl's libc binding does not expose renameat2; keep NOREPLACE atomic
        // so interrupted/repeated creates cannot overwrite an existing disk.
        if unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        sync(volumes)?;
        sync(&root.join(".pending"))?;
        Ok(())
    })();
    if staging.exists() {
        let _ = fs::remove_file(staging.join(name));
        let _ = fs::remove_dir(&staging);
    }
    result
}

pub(super) fn delete(volumes: &Path, name: &str) -> CubecowResult<()> {
    if !safe_name(name) {
        return Err(bad("invalid plain volume identity"));
    }
    let root = volumes
        .parent()
        .ok_or_else(|| bad("plain storage root is missing"))?;
    let dir = root.join(".trash").join(name);
    if !dir.exists() {
        let original = volumes.join(name);
        check_dir(&original)?;
        owned_file(&original.join(name))?;
        for entry in fs::read_dir(&original)? {
            let entry = entry?;
            if entry.file_name() != name
                && entry.file_name() != ".initialized"
                && entry.file_name() != ".initialized.pending"
            {
                return Err(bad("plain volume has unknown objects"));
            }
            owned_file(&entry.path())?;
        }
        fs::rename(&original, &dir)?;
        sync(volumes)?;
        sync(&root.join(".trash"))?;
    }
    remove_unpublished(&dir, Some(name))?;
    sync(&root.join(".trash"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::engine::{reflink::ReflinkEngine, Engine};
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::symlink;

    struct Pool(std::path::PathBuf);
    impl Pool {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("cubecow-plain-{}", uuid::Uuid::new_v4())))
        }
        fn engine(&self, reserve: u64) -> anyhow::Result<ReflinkEngine> {
            let config: AppConfig = serde_json::from_value(serde_json::json!({
                "log": {}, "backend": {"kind":"plain", "plain":{"root_dir":self.0,"reserve_bytes":reserve}}
            })).unwrap();
            ReflinkEngine::initialize_without_logging(config)
        }
    }
    impl Drop for Pool {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn persistent_independent_allocations_and_process_lock() {
        let pool = Pool::new();
        let engine = pool.engine(0).unwrap();
        assert!(pool.engine(0).is_err(), "one process must own the pool");
        let a = engine.create_volume("a", 1024 * 1024).unwrap();
        let b = engine.create_volume("b", 1024 * 1024).unwrap();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&a.device_path)
            .unwrap();
        file.write_all(b"persistent data").unwrap();
        file.sync_all().unwrap();
        let inode = file.metadata().unwrap().ino();
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o600);
        assert!(matches!(
            engine.create_volume("a", 1024),
            Err(CubecowError::AlreadyExists(_))
        ));
        assert!(fs::read(&b.device_path).unwrap().iter().all(|b| *b == 0));
        drop(engine);
        let engine = pool.engine(0).unwrap();
        assert_eq!(engine.get_volume_info("a").unwrap().size_bytes, 1024 * 1024);
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = [0; 15];
        file.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"persistent data");
        assert_eq!(file.metadata().unwrap().ino(), inode);
        engine.delete_volume("a").unwrap();
        engine.delete_volume("b").unwrap();
        assert_eq!(engine.list_volumes(0, None).2, 0);
    }

    #[test]
    fn unsupported_operations_and_capacity_fail_without_objects() {
        let pool = Pool::new();
        let engine = pool.engine(i64::MAX as u64).unwrap();
        assert!(engine.create_volume("full", 1024 * 1024).is_err());
        assert!(fs::read_dir(pool.0.join(".pending"))
            .unwrap()
            .next()
            .is_none());
        assert!(engine.create_snapshot_from_volume("a", "s", false).is_err());
        assert!(engine.create_volume_from_snapshot("s", "a").is_err());
        assert!(engine.resize_volume("a", 4096).is_err());
        assert!(engine.delete_snapshot("s").is_err());
        assert!(engine.reset_node_storage().is_err());
        assert!(engine.list_snapshots("a", 0, None).0.is_empty());
        assert_eq!(engine.list_volumes(0, None).2, 0);
    }

    #[test]
    fn interrupted_allocation_and_deletion_are_recovered() {
        let pool = Pool::new();
        let engine = pool.engine(0).unwrap();
        engine.create_volume("deleted", 4096).unwrap();
        drop(engine);
        fs::rename(
            pool.0.join("volumes/deleted"),
            pool.0.join(".trash/deleted"),
        )
        .unwrap();
        let staging = pool
            .0
            .join(".pending")
            .join(uuid::Uuid::new_v4().to_string());
        private_dir(&staging).unwrap();
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(staging.join("unfinished"))
            .unwrap();
        let engine = pool.engine(0).unwrap();
        assert_eq!(engine.list_volumes(0, None).2, 0);
        assert!(fs::read_dir(pool.0.join(".pending"))
            .unwrap()
            .next()
            .is_none());
        assert!(fs::read_dir(pool.0.join(".trash"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn unknown_symlink_sparse_or_unregistered_objects_are_refused() {
        let pool = Pool::new();
        let engine = pool.engine(0).unwrap();
        let volume = engine.create_volume("a", 4096).unwrap();
        engine.create_volume("healthy", 4096).unwrap();
        fs::remove_file(&volume.device_path).unwrap();
        symlink("/etc/passwd", &volume.device_path).unwrap();
        assert!(engine.get_volume_info("a").is_err());
        drop(engine);
        assert!(pool.engine(0).is_err());
        fs::remove_file(&volume.device_path).unwrap();
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&volume.device_path)
            .unwrap();
        file.set_len(4096).unwrap();
        let recovered = pool.engine(0).unwrap();
        assert!(recovered.get_volume_info("a").is_err());
        assert_eq!(
            recovered.get_volume_info("healthy").unwrap().size_bytes,
            4096
        );
        drop(recovered);
        fs::remove_file(pool.0.join(".plain-v1")).unwrap();
        assert!(pool.engine(0).is_err());
        assert!(
            Path::new(&volume.device_path).exists(),
            "unknown disk must be preserved"
        );
    }
}
