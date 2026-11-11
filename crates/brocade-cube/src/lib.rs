//! Cube-specific node execution. Console ownership and quotas remain in Store.
//! RPC types are generated from the pinned Cube and containerd protocol sources.

mod backend;
mod config;
mod files;
mod guest;
mod guest_cleanup;
mod lifecycle;
mod metrics;
mod network;
mod network_quota;
mod observation;
mod reconcile;
mod records;
mod rpc;
mod storage;
mod terminal;

pub use backend::{report, Backend};
pub use terminal::{TerminalCommand, TerminalEvent, TerminalSession};

pub type Result<T> = std::result::Result<T, String>;

#[allow(clippy::all, dead_code)]
pub(crate) mod proto {
    include!(concat!(env!("OUT_DIR"), "/generated.rs"));
}

pub(crate) fn text<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

pub(crate) fn number(value: &serde_json::Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

pub(crate) fn array<'a>(value: &'a serde_json::Value, key: &str) -> &'a [serde_json::Value] {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub(crate) fn successful(value: &serde_json::Value) -> Result<()> {
    if number(&value["ret"], "ret_code") == 200 {
        Ok(())
    } else {
        Err("Cube operation was not confirmed; reservation retained".into())
    }
}

pub(crate) fn now_millis() -> Result<u64> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch")?
        .as_millis();
    millis
        .try_into()
        .map_err(|_| "system clock exceeds supported range".into())
}
