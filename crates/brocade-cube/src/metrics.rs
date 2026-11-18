use crate::{backend::Inner, Result};
use brocade_deployment::cvm::*;
use std::time::Duration;

pub(crate) use brocade_deployment::cvm::parse_system_metrics as parse;

impl Inner {
    pub(crate) fn guest_metrics(
        &self,
        runtime: &str,
        interfaces: &[InterfaceObservation],
    ) -> Result<GuestMetrics> {
        let bytes = self.guest_exec(
            runtime,
            false,
            &["/bin/sh", "-c", SYSTEM_METRICS_SCRIPT],
            Duration::from_secs(4),
        )?;
        parse(&bytes, interfaces, crate::now_millis()?)
    }
}
