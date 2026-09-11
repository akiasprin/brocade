//! The same build supplies both node downloads and the local probe executable.

use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// Native Linux executables compiled from components/xray-core by build.rs.
pub const EMBEDDED_XRAYS: &[(&str, &[u8], &str)] = &[
    (
        "x86_64",
        include_bytes!(concat!(env!("OUT_DIR"), "/xray-x86_64")),
        env!("BROCADE_EMBEDDED_XRAY_SHA256_X86_64"),
    ),
    (
        "aarch64",
        include_bytes!(concat!(env!("OUT_DIR"), "/xray-aarch64")),
        env!("BROCADE_EMBEDDED_XRAY_SHA256_AARCH64"),
    ),
];

pub const BROCADE_XRAY_VERSION: &str = env!("BROCADE_EMBEDDED_XRAY_VERSION");

pub(crate) fn prepare(cache_root: &Path, arch: &str) -> Result<PathBuf, String> {
    let (_, bytes, digest) = EMBEDDED_XRAYS
        .iter()
        .find(|(candidate, ..)| *candidate == arch)
        .ok_or_else(|| format!("Console 没有适用于 {arch} 的内嵌拨测 Xray"))?;
    materialize(cache_root, arch, bytes, digest)
        .map_err(|error| format!("Console 无法准备内嵌拨测 Xray：{error}"))
}

fn materialize(cache_root: &Path, arch: &str, bytes: &[u8], digest: &str) -> io::Result<PathBuf> {
    // Verify build metadata before publishing executable bytes, including on a cache miss.
    if format!("{:x}", Sha256::digest(bytes)) != digest {
        return Err(io::Error::other("内嵌 Xray 与构建记录的 SHA256 不一致"));
    }
    let directory = cache_root.join("xray");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "Xray 缓存必须是服务账号所有的私有普通目录",
        ));
    }

    // A new release gets a new path. Existing probes can finish on their original inode.
    let destination = directory.join(format!("xray-{arch}-{digest}"));
    if verified_cache(&destination, bytes.len() as u64, digest)? {
        return Ok(destination);
    }
    let mut staging = tempfile::NamedTempFile::new_in(&directory)?;
    staging.write_all(bytes)?;
    staging
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o500))?;
    staging.as_file().sync_all()?;
    // Close the writable FD before rename. Another process may immediately execute the
    // published file, and Linux rejects that with ETXTBSY while a writer still holds it.
    staging.into_temp_path().persist(&destination)?;
    File::open(&directory)?.sync_all()?;
    Ok(destination)
}

fn verified_cache(path: &Path, length: u64, digest: &str) -> io::Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other(
            "Xray 缓存文件必须是服务账号所有的普通文件",
        ));
    }
    if metadata.mode() & 0o777 != 0o500 || metadata.len() != length {
        return Ok(false);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other("Xray 缓存文件在检查期间发生变化"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()) == digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, sync::Barrier};

    const SCRIPT: &[u8] = b"#!/bin/sh\nprintf 'Xray 26.4.25\\n'\n";

    fn prepare_fixture(root: &Path) -> PathBuf {
        materialize(
            root,
            "x86_64",
            SCRIPT,
            &format!("{:x}", Sha256::digest(SCRIPT)),
        )
        .unwrap()
    }

    #[test]
    fn cache_is_verified_repaired_and_versioned_without_overwriting_old_releases() {
        let root = tempfile::tempdir().unwrap();
        let path = prepare_fixture(root.path());
        let original_inode = fs::metadata(&path).unwrap().ino();
        assert_eq!(prepare_fixture(root.path()), path);
        assert_eq!(fs::metadata(&path).unwrap().ino(), original_inode);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o500);
        // Same length catches implementations that check only existence or size.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&path, vec![b'x'; SCRIPT.len()]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500)).unwrap();
        prepare_fixture(root.path());
        assert_eq!(fs::read(&path).unwrap(), SCRIPT);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        prepare_fixture(root.path());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o500);
        let next = b"next build";
        let new_path = materialize(
            root.path(),
            "x86_64",
            next,
            &format!("{:x}", Sha256::digest(next)),
        )
        .unwrap();
        assert_ne!(new_path, path);
        assert_eq!(fs::read(&path).unwrap(), SCRIPT);
        assert_eq!(fs::read(new_path).unwrap(), next);
    }

    #[test]
    fn unsafe_cache_paths_and_wrong_embedded_digest_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let digest = format!("{:x}", Sha256::digest(SCRIPT));
        assert!(materialize(root.path(), "x86_64", SCRIPT, "bad digest").is_err());
        assert!(!root.path().join("xray").exists());
        symlink(outside.path(), root.path().join("xray")).unwrap();
        assert!(materialize(root.path(), "x86_64", SCRIPT, &digest).is_err());
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
        fs::remove_file(root.path().join("xray")).unwrap();
        let path = prepare_fixture(root.path());
        fs::remove_file(&path).unwrap();
        let victim = outside.path().join("untouched");
        fs::write(&victim, "keep").unwrap();
        symlink(&victim, &path).unwrap();
        assert!(materialize(root.path(), "x86_64", SCRIPT, &digest).is_err());
        assert_eq!(fs::read_to_string(victim).unwrap(), "keep");
        assert!(prepare(root.path(), "riscv64").is_err());
    }

    #[test]
    fn concurrent_publication_never_exposes_a_partial_or_busy_executable() {
        let root = tempfile::tempdir().unwrap();
        let barrier = Barrier::new(8);
        std::thread::scope(|scope| {
            let mut tasks = Vec::new();
            for _ in 0..8 {
                tasks.push(scope.spawn(|| {
                    barrier.wait();
                    let path = prepare_fixture(root.path());
                    let output = std::process::Command::new(&path).output().unwrap();
                    assert!(output.status.success());
                    assert_eq!(output.stdout, b"Xray 26.4.25\n");
                    path
                }));
            }
            let paths: Vec<_> = tasks.into_iter().map(|task| task.join().unwrap()).collect();
            assert!(paths.iter().all(|path| path == &paths[0]));
        });
        assert_eq!(fs::read_dir(root.path().join("xray")).unwrap().count(), 1);
    }
}
