//! One in-process Cube executor shared by convergence, observations and terminals.
use brocade_cube::Backend;
use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

static BACKEND: OnceLock<Mutex<Option<Backend>>> = OnceLock::new();

pub(super) fn configured() -> Result<bool, String> {
    let explicit = std::env::var_os("BROCADE_CVM_CONFIG");
    let path = explicit
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| "/etc/brocade/cvm-cube.json".into());
    let exists = path
        .try_exists()
        .map_err(|_| "CVM configuration cannot be inspected")?;
    if explicit.is_some() && !exists {
        return Err("CVM configured executor is unavailable".into());
    }
    Ok(exists)
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
    let config = std::env::var_os("BROCADE_CVM_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/brocade/cvm-cube.json"));
    let state = std::env::var_os("BROCADE_CVM_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/brocade/cvm-cube"));
    if !config
        .try_exists()
        .map_err(|_| "CVM configuration cannot be inspected")?
    {
        return Err("CVM Rust executor configuration is unavailable".into());
    }
    let service = Backend::open(&config, &state)?;
    *backend = Some(service.clone());
    Ok(service)
}
