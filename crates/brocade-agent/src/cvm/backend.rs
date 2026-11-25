//! One Cube executor with an optional physical-host executor in the same Agent process.
use brocade_deployment::cvm::*;

#[derive(Clone)]
enum HostBackend {
    Disabled,
    Ready(super::host::Backend),
    Failed(String),
}

#[derive(Clone)]
pub(super) struct Backend {
    cube: brocade_cube::Backend,
    host: HostBackend,
}

impl Backend {
    pub(super) fn image_manifests(&self) -> Vec<CvmImageManifest> {
        self.cube.image_manifests()
    }

    pub(super) fn inventory(&self) -> Result<Capabilities, String> {
        let mut capabilities = self.cube.inventory()?;
        match &self.host {
            HostBackend::Disabled => {}
            HostBackend::Ready(host) => match host.system_inventory().and_then(|inventory| {
                let mut composite = capabilities.clone();
                host.augment_capabilities(&mut composite)?;
                Ok((inventory, composite))
            }) {
                Ok((inventory, composite)) => {
                    capabilities = composite;
                    capabilities.host_system = Some(inventory);
                }
                Err(error) => {
                    capabilities.host_system_error = Some(error.chars().take(120).collect())
                }
            },
            HostBackend::Failed(error) => {
                capabilities.host_system_error = Some(error.chars().take(120).collect());
            }
        }
        capabilities.validate()?;
        Ok(capabilities)
    }

    pub(super) fn stage_credential(&self, credential: &GuestCredential) -> Result<(), String> {
        self.cube.stage_credential(credential)
    }

    fn host(&self) -> Result<&super::host::Backend, String> {
        match &self.host {
            HostBackend::Ready(host) => Ok(host),
            HostBackend::Disabled => Err("physical host execution is not configured".into()),
            HostBackend::Failed(error) => Err(error.clone()),
        }
    }

    pub(super) fn submit(&self, desired: &InstanceDesired) -> Result<InstanceReport, String> {
        if desired.spec.execution.is_cube() {
            self.cube.submit(desired)
        } else {
            self.host()?.submit(desired)
        }
    }

    pub(super) fn observe(
        &self,
        targets: &[ObservationTarget],
        ids: &[String],
    ) -> Result<Vec<InstanceObservation>, String> {
        let host = match &self.host {
            HostBackend::Ready(host) => Some(host),
            HostBackend::Disabled | HostBackend::Failed(_) => None,
        };
        let mut host_targets = Vec::new();
        let mut cube_targets = Vec::new();
        for target in targets {
            let host_owned = match host {
                Some(host) => host.owns(&target.id)?,
                None => false,
            };
            if host_owned {
                host_targets.push(target.clone());
            } else {
                cube_targets.push(target.clone());
            }
        }
        let mut host_ids = std::collections::BTreeSet::new();
        if let Some(host) = host {
            for id in ids {
                if host.owns(id)? {
                    host_ids.insert(id.clone());
                }
            }
        }
        let cube_ids = ids
            .iter()
            .filter(|id| !host_ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        let mut observations = self.cube.observe(&cube_targets, &cube_ids)?;
        if !host_targets.is_empty() {
            let host_ids = host_ids.into_iter().collect::<Vec<_>>();
            observations.extend(self.host()?.observe(&host_targets, &host_ids)?);
        }
        Ok(observations)
    }

    pub(super) fn persistent_terminal(
        &self,
        offer: terminal::Offer,
    ) -> Result<brocade_cube::TerminalSession, String> {
        let host_owned = match &self.host {
            HostBackend::Ready(host) => host.owns(&offer.target.instance_id)?,
            HostBackend::Disabled | HostBackend::Failed(_) => false,
        };
        if host_owned {
            self.host()?.persistent_terminal(offer)
        } else {
            self.cube.persistent_terminal(offer)
        }
    }

    pub(super) fn revoke_persistent_terminals(&self, through: u64) -> Result<(), String> {
        let cube = self.cube.revoke_persistent_terminals(through);
        let host = match &self.host {
            HostBackend::Ready(host) => host.revoke_persistent_terminals(through),
            HostBackend::Disabled | HostBackend::Failed(_) => Ok(()),
        };
        cube.and(host)
    }

    pub(super) fn supports_network_quotas(&self) -> bool {
        self.cube.supports_network_quotas()
    }

    pub(super) fn apply_network_quotas(
        &self,
        desired: &NetworkQuotaDesired,
    ) -> Result<Vec<NetworkQuotaReport>, String> {
        self.cube.apply_network_quotas(desired)
    }

    #[cfg(test)]
    pub(super) fn wait_idle(&self, timeout: std::time::Duration) -> Result<(), String> {
        self.cube.wait_idle(timeout)
    }

    #[cfg(test)]
    pub(super) fn collect_observations(&self) -> Result<(), String> {
        self.cube.collect_observations()
    }
}

pub(super) fn inspect(config: &std::path::Path) -> Result<Capabilities, String> {
    brocade_cube::Backend::inspect(config)
}

pub(super) fn state_path() -> Result<std::path::PathBuf, String> {
    Ok(std::env::var_os("BROCADE_CVM_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/var/lib/brocade/cvm-cube".into()))
}

pub(super) fn config_path() -> Result<std::path::PathBuf, String> {
    Ok(std::env::var_os("BROCADE_CVM_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/etc/brocade/cvm-cube.json".into()))
}

pub(super) fn host_config_path() -> Result<Option<std::path::PathBuf>, String> {
    let Some(path) = std::env::var_os("BROCADE_CVM_HOST_CONFIG").map(std::path::PathBuf::from)
    else {
        return Ok(None);
    };
    if !path.is_absolute() {
        return Err("host management configuration must use an absolute path".into());
    }
    Ok(Some(path))
}

pub(super) fn host_state_path() -> Result<std::path::PathBuf, String> {
    let path = std::env::var_os("BROCADE_CVM_HOST_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/var/lib/brocade/cvm-host".into());
    if !path.is_absolute() {
        return Err("host management state must use an absolute path".into());
    }
    Ok(path)
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
    let config = config_path()?;
    let state = state_path()?;
    if !config
        .try_exists()
        .map_err(|_| "CVM configuration cannot be inspected")?
    {
        return Err("CVM Rust executor configuration is unavailable".into());
    }
    let cube = brocade_cube::Backend::open(&config, &state)?;
    let host = match host_config_path()? {
        None => HostBackend::Disabled,
        Some(config) => match super::host::Backend::open(&config, &host_state_path()?) {
            Ok(host) => HostBackend::Ready(host),
            Err(error) => HostBackend::Failed(error),
        },
    };
    let service = Backend { cube, host };
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
