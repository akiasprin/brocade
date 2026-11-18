//! One in-process Cube executor shared by convergence, observations and terminals.
use brocade_deployment::cvm::*;
#[derive(Clone)]
pub(super) enum Backend {
    Cube(brocade_cube::Backend),
    Host(super::host::Backend),
}
impl Backend {
    pub(super) fn image_manifests(&self) -> Vec<CvmImageManifest> {
        match self {
            Self::Cube(b) => b.image_manifests(),
            Self::Host(_) => Vec::new(),
        }
    }
    pub(super) fn inventory(&self) -> Result<Capabilities, String> {
        match self {
            Self::Cube(b) => b.inventory(),
            Self::Host(b) => b.inventory(),
        }
    }
    pub(super) fn submit(&self, d: &InstanceDesired) -> Result<InstanceReport, String> {
        match self {
            Self::Cube(b) => b.submit(d),
            Self::Host(b) => b.submit(d),
        }
    }
    pub(super) fn observe(
        &self,
        t: &[ObservationTarget],
        ids: &[String],
    ) -> Result<Vec<InstanceObservation>, String> {
        match self {
            Self::Cube(b) => b.observe(t, ids),
            Self::Host(b) => b.observe(t, ids),
        }
    }
    pub(super) fn persistent_terminal(
        &self,
        o: terminal::Offer,
    ) -> Result<brocade_cube::TerminalSession, String> {
        match self {
            Self::Cube(b) => b.persistent_terminal(o),
            Self::Host(b) => b.persistent_terminal(o),
        }
    }
    pub(super) fn revoke_persistent_terminals(&self, through: u64) -> Result<(), String> {
        match self {
            Self::Cube(b) => b.revoke_persistent_terminals(through),
            Self::Host(b) => b.revoke_persistent_terminals(through),
        }
    }
    pub(super) fn supports_network_quotas(&self) -> bool {
        match self {
            Self::Cube(b) => b.supports_network_quotas(),
            Self::Host(_) => false,
        }
    }
    pub(super) fn apply_network_quotas(
        &self,
        d: &NetworkQuotaDesired,
    ) -> Result<Vec<NetworkQuotaReport>, String> {
        match self {
            Self::Cube(b) => b.apply_network_quotas(d),
            Self::Host(_) => Err("native connection quota is not available".into()),
        }
    }
    #[cfg(test)]
    pub(super) fn wait_idle(&self, t: std::time::Duration) -> Result<(), String> {
        match self {
            Self::Cube(b) => b.wait_idle(t),
            Self::Host(_) => Ok(()),
        }
    }
    #[cfg(test)]
    pub(super) fn collect_observations(&self) -> Result<(), String> {
        match self {
            Self::Cube(b) => b.collect_observations(),
            Self::Host(_) => Ok(()),
        }
    }
}
pub(super) fn native() -> Result<bool, String> {
    match std::env::var("BROCADE_CVM_BACKEND").as_deref() {
        Ok("baremetal") => Ok(true),
        Ok("cube") | Err(std::env::VarError::NotPresent) => Ok(false),
        _ => Err("BROCADE_CVM_BACKEND must be cube or baremetal".into()),
    }
}
pub(super) fn inspect(config: &std::path::Path) -> Result<Capabilities, String> {
    if native()? {
        super::host::Backend::inspect(config)
    } else {
        brocade_cube::Backend::inspect(config)
    }
}
pub(super) fn state_path() -> Result<std::path::PathBuf, String> {
    let native = native()?;
    Ok(std::env::var_os("BROCADE_CVM_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            if native {
                "/var/lib/brocade/cvm-host".into()
            } else {
                "/var/lib/brocade/cvm-cube".into()
            }
        }))
}
pub(super) fn config_path() -> Result<std::path::PathBuf, String> {
    let native = native()?;
    Ok(std::env::var_os("BROCADE_CVM_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            if native {
                "/etc/brocade/cvm-host.json".into()
            } else {
                "/etc/brocade/cvm-cube.json".into()
            }
        }))
}

use std::sync::{Mutex, OnceLock};

static BACKEND: OnceLock<Mutex<Option<Backend>>> = OnceLock::new();

pub(super) fn configured() -> Result<bool, String> {
    let explicit = std::env::var_os("BROCADE_CVM_CONFIG");
    let path = config_path()?;
    configured_path(&path, explicit.is_some())
}

fn configured_path(path: &std::path::Path, explicit: bool) -> Result<bool, String> {
    // A missing explicitly selected executor is an unavailable configuration,
    // not a disabled one. Keep reporting its inventory error and held instances.
    if explicit {
        return Ok(true);
    }
    path.try_exists()
        .map_err(|_| "CVM configuration cannot be inspected".into())
}

pub(super) fn current() -> Option<Backend> {
    BACKEND.get()?.try_lock().ok()?.clone()
}

pub(super) fn get() -> Result<Backend, String> {
    if super::maintenance()? {
        return Err("CVM 虚拟化模式切换中，等待宿主重启或完成恢复".into());
    }
    let mut backend = BACKEND
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "CVM executor initialization unavailable")?;
    if let Some(backend) = backend.as_ref() {
        return Ok(backend.clone());
    }
    let is_native = native()?;
    let config = config_path()?;
    let state = state_path()?;
    if !config
        .try_exists()
        .map_err(|_| "CVM configuration cannot be inspected")?
    {
        return Err("CVM Rust executor configuration is unavailable".into());
    }
    let service = if is_native {
        Backend::Host(super::host::Backend::open(&config, &state)?)
    } else {
        Backend::Cube(brocade_cube::Backend::open(&config, &state)?)
    };
    *backend = Some(service.clone());
    Ok(service)
}

#[cfg(test)]
mod tests {
    #[test]
    fn explicit_missing_executor_still_reports_configured() {
        let path = std::env::temp_dir().join(format!(
            "brocade-missing-cvm-executor-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!path.exists());
        assert_eq!(super::configured_path(&path, true), Ok(true));
        assert_eq!(super::configured_path(&path, false), Ok(false));
        assert!(
            !path.exists(),
            "configuration probing must remain read-only"
        );
    }
}
