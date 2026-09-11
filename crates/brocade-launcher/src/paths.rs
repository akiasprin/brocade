use anyhow::{bail, Context, Result};
use std::{env, fs, io::Write, path::PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct LauncherPaths {
    pub(crate) data: PathBuf,
    pub(crate) cache: PathBuf,
    pub(crate) runtime: PathBuf,
}

impl LauncherPaths {
    pub(crate) fn resolve(data: Option<PathBuf>, cache: Option<PathBuf>) -> Result<Self> {
        let data = resolve_root(data, "BROCADE_DATA_DIR", "XDG_DATA_HOME", ".local/share")?;
        let cache = resolve_root(cache, "BROCADE_CACHE_DIR", "XDG_CACHE_HOME", ".cache")?;
        let runtime = data.join("run");
        ensure_private_dir(&data)?;
        ensure_private_dir(&cache)?;
        ensure_private_dir(&runtime)?;
        Ok(Self {
            data,
            cache,
            runtime,
        })
    }
}

fn resolve_root(
    explicit: Option<PathBuf>,
    brocade_env: &str,
    xdg_env: &str,
    home_suffix: &str,
) -> Result<PathBuf> {
    let selected = explicit
        .or_else(|| nonempty_env(brocade_env).map(PathBuf::from))
        .or_else(|| nonempty_env(xdg_env).map(|root| PathBuf::from(root).join("brocade")))
        .or_else(|| {
            nonempty_env("HOME").map(|root| PathBuf::from(root).join(home_suffix).join("brocade"))
        })
        .with_context(|| {
            format!("无法确定 Brocade 目录；请设置 {brocade_env} 或在 brocade up 中传入对应目录")
        })?;
    if selected.is_absolute() {
        Ok(selected)
    } else {
        Ok(env::current_dir()?.join(selected))
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

pub(crate) fn ensure_private_dir(path: &std::path::Path) -> Result<()> {
    #[cfg(not(unix))]
    bail!("brocade up 当前只支持 Unix 权限模型");

    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

        if !path.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .with_context(|| format!("无法创建私有目录 {}", path.display()))?;
        }
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            bail!("{} 必须是普通目录，不能是符号链接", path.display());
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("{} 必须属于当前用户", path.display());
        }
        if metadata.mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

pub(crate) fn atomic_write_private(path: &std::path::Path, bytes: &[u8], mode: u32) -> Result<()> {
    #[cfg(not(unix))]
    bail!("brocade up 当前只支持 Unix 权限模型");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let parent = path.parent().context("目标文件没有父目录")?;
        ensure_private_dir(parent)?;
        let mut staging = tempfile::NamedTempFile::new_in(parent)?;
        staging.write_all(bytes)?;
        staging
            .as_file()
            .set_permissions(fs::Permissions::from_mode(mode))?;
        staging.as_file().sync_all()?;
        staging
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("无法发布 {}", path.display()))?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

pub(crate) fn read_private_text(path: &std::path::Path) -> Result<String> {
    #[cfg(not(unix))]
    bail!("brocade up 当前只支持 Unix 权限模型");

    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            bail!("{} 必须是当前用户所有的 0600 普通文件", path.display());
        }
        let mut value = String::new();
        file.read_to_string(&mut value)?;
        Ok(value)
    }
}

pub(crate) struct InstanceLock {
    _file: fs::File,
}

impl InstanceLock {
    pub(crate) fn acquire(runtime: &std::path::Path) -> Result<Self> {
        #[cfg(not(unix))]
        bail!("brocade up 当前只支持 Unix 权限模型");

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

            let path = runtime.join("launcher.lock");
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                bail!("{} 必须是当前用户所有的 0600 普通文件", path.display());
            }
            file.try_lock()
                .context("同一运行目录已有一个 brocade up 正在运行")?;
            Ok(Self { _file: file })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_roots_are_created_private() {
        let root = tempfile::tempdir().unwrap();
        let paths = LauncherPaths::resolve(
            Some(root.path().join("state")),
            Some(root.path().join("cache")),
        )
        .unwrap();
        assert_eq!(paths.data, root.path().join("state"));
        assert_eq!(paths.cache, root.path().join("cache"));
        assert!(paths.runtime.is_dir());
    }
}
