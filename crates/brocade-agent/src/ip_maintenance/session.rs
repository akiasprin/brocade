use crate::fsutil::{atomic_write_private, read_bounded};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

pub(super) struct Session {
    pub jar: Arc<CookieStoreMutex>,
    path: PathBuf,
}

pub(super) fn clear(directory: &Path) -> Result<(), String> {
    let folder = directory.join("cookies");
    let metadata = match fs::symlink_metadata(&folder) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("cookie_metadata".into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("unexpected_cookie_entry".into());
    }
    let entries = fs::read_dir(&folder)
        .map_err(|_| "cookie_inventory")?
        .take(129)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "cookie_inventory")?;
    if entries.len() > 128 {
        return Err("cookie_file_limit".into());
    }
    for entry in &entries {
        if !entry.file_type().map_err(|_| "cookie_metadata")?.is_file() {
            return Err("unexpected_cookie_entry".into());
        }
    }
    for entry in entries {
        fs::remove_file(entry.path()).map_err(|_| "cookie_cleanup")?;
    }
    Ok(())
}
impl Session {
    pub fn for_service(
        &self,
        directory: &Path,
        service: &str,
        idle_days: u32,
    ) -> Result<Self, String> {
        let identity = self
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or("cookie_identity")?;
        Self::open(directory, &format!("{identity}|{service}"), idle_days)
    }
    pub fn open(directory: &Path, identity: &str, idle_days: u32) -> Result<Self, String> {
        let folder = directory.join("cookies");
        fs::create_dir_all(&folder).map_err(|_| "cookie_directory")?;
        let mut files = Vec::new();
        for entry in fs::read_dir(&folder)
            .map_err(|_| "cookie_inventory")?
            .take(129)
        {
            let entry = entry.map_err(|_| "cookie_inventory")?;
            let metadata = entry.metadata().map_err(|_| "cookie_metadata")?;
            if !entry.file_type().map_err(|_| "cookie_metadata")?.is_file() {
                return Err("unexpected_cookie_entry".into());
            }
            let modified = metadata.modified().map_err(|_| "cookie_time")?;
            let old = SystemTime::now()
                .duration_since(modified)
                .is_ok_and(|age| age > Duration::from_secs(u64::from(idle_days) * 86400));
            if old {
                fs::remove_file(entry.path()).map_err(|_| "cookie_cleanup")?;
            } else {
                files.push((modified, entry.path()));
            }
        }
        if files.len() > 128 {
            return Err("cookie_file_limit".into());
        }
        files.sort_by_key(|(time, _)| *time);
        let excess = files.len().saturating_sub(15);
        for (_, path) in files.into_iter().take(excess) {
            fs::remove_file(path).map_err(|_| "cookie_cleanup")?;
        }
        let path = folder.join(format!(
            "{}.jsonl",
            brocade_core::hash::sha256_hex(identity.as_bytes())
        ));
        let store = match read_bounded(&path, 1024 * 1024)? {
            Some(bytes) => CookieStore::load(Cursor::new(bytes), |line| serde_json::from_str(line))
                .map_err(|_| "cookie_checkpoint_invalid")?,
            None => CookieStore::default(),
        };
        Ok(Self {
            jar: Arc::new(CookieStoreMutex::new(store)),
            path,
        })
    }
    pub fn save(&self) -> Result<(), String> {
        let store = self.jar.lock().map_err(|_| "cookie_lock")?;
        if store.iter_any().count() > 256 {
            return Err("cookie_count_limit".into());
        }
        let mut bytes = Vec::new();
        // Unlike CookieStore's Serialize implementation, this preserves session cookies too.
        store
            .save_incl_expired_and_nonpersistent(&mut bytes, serde_json::to_string)
            .map_err(|_| "cookie_checkpoint_encode")?;
        if bytes.len() > 1024 * 1024 {
            return Err("cookie_size_limit".into());
        }
        atomic_write_private(&self.path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn persists_session_cookies_with_private_mode_and_isolates_module_identity() {
        let directory = std::env::temp_dir().join(format!(
            "brocade-maintenance-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let url = reqwest::Url::parse("https://www.google.com/").unwrap();
        let first = Session::open(&directory, "fixture-google-ip", 14).unwrap();
        first
            .jar
            .lock()
            .unwrap()
            .parse("session=fixture-state; Path=/; Secure", &url)
            .unwrap();
        first.save().unwrap();
        assert_eq!(
            fs::metadata(&first.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let restored = Session::open(&directory, "fixture-google-ip", 14).unwrap();
        assert!(
            reqwest::cookie::CookieStore::cookies(restored.jar.as_ref(), &url)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("fixture-state")
        );
        let other = Session::open(&directory, "fixture-trust-ip", 14).unwrap();
        assert!(reqwest::cookie::CookieStore::cookies(other.jar.as_ref(), &url).is_none());
        fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod reset_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn clear_rejects_symlinks_before_removing_any_session_file() {
        let root = std::env::temp_dir().join(format!(
            "brocade-cookie-reset-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), b"kept").unwrap();
        symlink(&outside, root.join("cookies")).unwrap();
        assert_eq!(clear(&root).unwrap_err(), "unexpected_cookie_entry");
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"kept");
        fs::remove_file(root.join("cookies")).unwrap();
        fs::create_dir(root.join("cookies")).unwrap();
        fs::write(
            root.join("cookies/a.jsonl"),
            b"kept until inventory is valid",
        )
        .unwrap();
        symlink(outside.join("keep"), root.join("cookies/z.jsonl")).unwrap();
        assert_eq!(clear(&root).unwrap_err(), "unexpected_cookie_entry");
        assert!(root.join("cookies/a.jsonl").is_file());
        assert!(outside.join("keep").is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
