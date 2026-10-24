//! Cache discovery, never liveness. PID/start ticks, name and managed config are rechecked on
//! every lookup; recovery and retirement still enumerate all generations independently.
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use crate::ProcessRef;

const MAX_IDENTITIES: usize = 8;

#[derive(Default)]
struct ProcessCache(Vec<(Option<PathBuf>, ProcessRef)>);

impl ProcessCache {
    fn resolve(
        &mut self,
        key: Option<PathBuf>,
        valid: impl FnOnce(ProcessRef) -> bool,
        discover: impl FnOnce() -> Option<ProcessRef>,
    ) -> Option<ProcessRef> {
        if let Some(index) = self.0.iter().position(|(saved, _)| *saved == key) {
            let process = self.0[index].1;
            if valid(process) {
                return Some(process);
            }
            self.0.remove(index);
        }
        let process = discover()?;
        if self.0.len() == MAX_IDENTITIES {
            self.0.remove(0);
        }
        self.0.push((key, process));
        Some(process)
    }
}

pub(crate) fn serving(state_dir: Option<&Path>) -> Option<ProcessRef> {
    static CACHE: OnceLock<Mutex<ProcessCache>> = OnceLock::new();
    let expected = match state_dir {
        Some(directory) => Some(std::fs::canonicalize(directory).ok()?.join("xray.json")),
        None => None,
    };
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .resolve(
            expected.clone(),
            |process| {
                crate::process_ref(process.pid) == Some(process)
                    && std::fs::read_to_string(format!("/proc/{}/comm", process.pid))
                        .is_ok_and(|name| name.trim() == "xray")
                    && crate::xray_process_uses_config(process, expected.as_deref())
            },
            || {
                crate::serving_xray_processes_for(state_dir)
                    .into_iter()
                    .next()
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_identity_is_validated_without_repeating_discovery() {
        let mut cache = ProcessCache::default();
        let process = ProcessRef {
            pid: 12,
            start_ticks: 34,
        };
        let key = Some(PathBuf::from("/state/xray.json"));
        assert_eq!(
            cache.resolve(key.clone(), |_| panic!("empty cache"), || Some(process)),
            Some(process)
        );
        for _ in 0..10 {
            assert_eq!(
                cache.resolve(
                    key.clone(),
                    |saved| saved == process,
                    || panic!("healthy lookup must not scan /proc")
                ),
                Some(process)
            );
        }
    }

    #[test]
    fn exited_or_reused_pid_is_rediscovered_and_absence_is_not_cached() {
        let mut cache = ProcessCache::default();
        let old = ProcessRef {
            pid: 12,
            start_ticks: 34,
        };
        let replacement = ProcessRef {
            pid: 12,
            start_ticks: 56,
        };
        assert_eq!(cache.resolve(None, |_| false, || Some(old)), Some(old));
        assert_eq!(
            cache.resolve(None, |saved| saved == replacement, || Some(replacement)),
            Some(replacement)
        );
        assert_eq!(cache.resolve(None, |_| false, || None), None);
        assert_eq!(
            cache.resolve(None, |_| panic!("absence must not be cached"), || Some(old)),
            Some(old)
        );
    }

    #[test]
    fn managed_directories_are_isolated_and_cache_is_bounded() {
        let mut cache = ProcessCache::default();
        for i in 0..MAX_IDENTITIES + 3 {
            let process = ProcessRef {
                pid: i as i32 + 1,
                start_ticks: i as u64,
            };
            assert_eq!(
                cache.resolve(
                    Some(PathBuf::from(i.to_string())),
                    |_| false,
                    || Some(process)
                ),
                Some(process)
            );
            assert!(cache.0.len() <= MAX_IDENTITIES);
        }
        assert_eq!(cache.0.len(), MAX_IDENTITIES);
        assert_eq!(cache.0[0].0, Some(PathBuf::from("3")));
    }
    #[test]
    fn cached_managed_process_tracks_real_exit_and_ignores_probe_children() {
        use std::{
            io::Read,
            process::{Command, Stdio},
        };
        struct Fixture {
            children: Vec<std::process::Child>,
            root: PathBuf,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                for child in &mut self.children {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
        let root =
            std::env::temp_dir().join(format!("brocade-process-cache-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let binary = root.join("xray");
        std::fs::copy("/bin/sh", &binary).unwrap();
        std::fs::write(root.join("wait.sh"), "printf r\nread ignored\n").unwrap();
        let mut fixture = Fixture {
            children: Vec::new(),
            root: root.clone(),
        };
        let mut spawn = |config: &str| {
            let mut child = Command::new(&binary)
                .arg(root.join("wait.sh"))
                .arg("-config")
                .arg(root.join(config))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut ready = [0];
            child
                .stdout
                .as_mut()
                .unwrap()
                .read_exact(&mut ready)
                .unwrap();
            let process = crate::process_ref(child.id() as i32).unwrap();
            fixture.children.push(child);
            process
        };
        let probe = spawn("probe.json");
        let old = spawn("xray.json");
        let replacement = spawn("xray.json");
        assert_ne!(probe, old);
        for _ in 0..5 {
            assert_eq!(serving(Some(&root)), Some(old));
        }
        fixture.children[1].kill().unwrap();
        fixture.children[1].wait().unwrap();
        assert_eq!(serving(Some(&root)), Some(replacement));
        assert_eq!(crate::serving_xray_processes_in(&root), vec![replacement]);
    }
}
