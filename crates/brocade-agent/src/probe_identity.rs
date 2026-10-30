//! Authenticated Console identity for diagnostic evidence produced by this machine's Xray.
use std::path::{Path, PathBuf};

pub(crate) const HEADER: &str = brocade_deployment::unlock_probe::PROBE_NODE_ID_HEADER;
pub(crate) const ENV: &str = brocade_deployment::unlock_probe::PROBE_IDENTITY_ENV;
pub(crate) const PROFILE_ENV: &str = brocade_deployment::unlock_probe::PROBE_PROFILE_ENV;

pub(crate) fn ready(
    state_dir: &Path,
    version: Option<&str>,
    installed: Option<&str>,
    running: Option<&str>,
) -> bool {
    if installed.is_none()
        || installed != running
        || !version.is_some_and(|value| {
            crate::xray_version_has_capability(value, "probe-connection-evidence-v2")
        })
    {
        return false;
    }
    if !path(state_dir).is_file() {
        return false;
    }
    let Some(process) = crate::xray_process::serving(Some(state_dir)) else {
        return false;
    };
    let Ok(environment) = std::fs::read(format!("/proc/{}/environ", process.pid)) else {
        return false;
    };
    let has_keys = [ENV, PROFILE_ENV].iter().all(|key| {
        environment
            .split(|byte| *byte == 0)
            .any(|entry| entry.starts_with(key.as_bytes()) && entry.get(key.len()) == Some(&b'='))
    });
    let profile = environment
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(format!("{PROFILE_ENV}=").as_bytes()));
    has_keys
        && profile
            .filter(|value| value.len() <= 32768)
            .and_then(|value| {
                serde_json::from_slice::<brocade_deployment::unlock_probe::UnlockProbeProfile>(
                    value,
                )
                .ok()
            })
            .is_some_and(|profile| {
                profile.fingerprint.len() == 64 && !profile.strategies.is_empty()
            })
}

pub(crate) fn profile(config: &str) -> Result<String, String> {
    let config: serde_json::Value =
        serde_json::from_str(config).map_err(|_| "invalid Xray profile for probe provenance")?;
    let mut profile = brocade_deployment::unlock_probe::UnlockProbeProfile::from_config(&config)
        .ok_or("missing probe provenance profile")?;
    let encoded =
        serde_json::to_string(&profile).map_err(|_| "invalid probe provenance profile")?;
    if encoded.len() <= 32768 {
        return Ok(encoded);
    }
    // Optional evidence must never exceed exec's environment/argument limits and
    // prevent a large serving profile from starting.
    eprintln!("probe provenance: strategy map exceeds 32 KiB; evidence capability withheld");
    profile.strategies.clear();
    profile.bindings.clear();
    serde_json::to_string(&profile).map_err(|_| "invalid bounded probe profile".into())
}

pub(crate) fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("probe-node-id")
}

pub(crate) fn observe(state_dir: &Path, value: Option<&str>) -> Result<(), String> {
    let Some(value) = value else { return Ok(()) };
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":._-".contains(&b))
    {
        return Err("invalid authenticated node identity".into());
    }
    let path = path(state_dir);
    if std::fs::read_to_string(&path).is_ok_and(|current| current == value) {
        return Ok(());
    }
    crate::fsutil::atomic_write_with_mode(&path, value.as_bytes(), 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn node_identity_is_bounded_and_not_a_shell_or_path_fragment() {
        let dir =
            std::env::temp_dir().join(format!("brocade-probe-identity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for value in ["", "../node", "node\nother", "$(command)"] {
            assert!(observe(&dir, Some(value)).is_err());
        }
        observe(&dir, Some("node-a")).unwrap();
        assert_eq!(std::fs::read_to_string(path(&dir)).unwrap(), "node-a");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn large_profiles_withhold_only_optional_evidence_and_bound_the_launch_environment() {
        let outbounds=(0..2000).map(|i|serde_json::json!({"tag":format!("out:{}:{i}","x".repeat(80)),"protocol":"freedom","settings":{"domainStrategy":"UseIP"}})).collect::<Vec<_>>();
        let input = serde_json::json!({"dns":{"servers":["localhost"]},"outbounds":outbounds});
        let encoded = profile(&input.to_string()).unwrap();
        assert!(encoded.len() < 32768);
        let profile: brocade_deployment::unlock_probe::UnlockProbeProfile =
            serde_json::from_str(&encoded).unwrap();
        assert!(profile.strategies.is_empty());
        assert_eq!(input["outbounds"].as_array().unwrap().len(), 2000);
    }
}
