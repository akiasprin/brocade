//! Agent attachment handles. Persistent Guest processes and I/O belong to CubeShim.
use crate::{
    backend::{running, Backend, Inner},
    Result,
};
use brocade_deployment::{cvm::terminal as wire, cvm::PowerState};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
mod persistent;

pub enum TerminalCommand {
    Data(Vec<u8>),
    Control(wire::Input),
}
pub enum TerminalEvent {
    Data(Vec<u8>),
    Output(wire::Output),
}

pub struct TerminalSession {
    pub input: tokio::sync::mpsc::Sender<TerminalCommand>,
    pub output: tokio::sync::mpsc::Receiver<TerminalEvent>,
    cancelled: Arc<AtomicBool>,
}
impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl Inner {
    fn terminal_identity(&self, target: &wire::Target, opening: bool) -> Result<String> {
        let state = self.state()?;
        let record = state
            .records
            .get(&target.instance_id)
            .ok_or("Guest terminal instance unavailable")?;
        let report = record
            .report
            .as_ref()
            .ok_or("Guest terminal runtime has not been observed")?;
        if record.deleted
            || record.runtime != target.runtime_id
            || record.restart_epoch != target.restart_epoch
            || record.reinstall_epoch != target.reinstall_epoch
            || !record.transition.is_empty()
            || report.power != Some(PowerState::Running)
        {
            return Err("Guest runtime has changed or stopped".into());
        }
        if state.active.as_ref().is_some_and(|d| {
            d.id == target.instance_id
                && (opening
                    || d.power != PowerState::Running
                    || d.restart_epoch != target.restart_epoch
                    || d.reinstall_epoch != target.reinstall_epoch)
        }) {
            return Err("Guest lifecycle is changing".into());
        }
        if state.pending.get(&target.instance_id).is_some_and(|d| {
            d.power != PowerState::Running
                || d.restart_epoch != target.restart_epoch
                || d.reinstall_epoch != target.reinstall_epoch
        }) {
            return Err("Guest lifecycle is changing".into());
        }
        Ok(record.launch_sha256.clone())
    }

    fn terminal_runtime(&self, target: &wire::Target, launch: &str) -> Result<()> {
        let response = self.cube_timeout(
            "List",
            json!({"id":target.runtime_id}),
            Duration::from_secs(2),
        )?;
        let items = crate::array(&response, "items");
        if items.len() != 1
            || crate::text(&items[0], "id") != target.runtime_id
            || !running(&items[0])
            || crate::text(&items[0]["labels"], "brocade.cvm.instance") != target.instance_id
            || crate::text(&items[0]["labels"], "brocade.cvm.spec_sha256") != launch
        {
            return Err("Guest terminal runtime ownership changed".into());
        }
        Ok(())
    }
}
