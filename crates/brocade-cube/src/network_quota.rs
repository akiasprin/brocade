//! Privileged node-local network assignments share the existing Cubelet RPC.
use crate::{array, number, text, Backend, Result};
use brocade_deployment::cvm::*;
use serde_json::json;

impl Backend {
    pub fn supports_network_quotas(&self) -> bool {
        self.0
            .inner
            .quota_supported
            .load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn apply_network_quotas(
        &self,
        desired: &NetworkQuotaDesired,
    ) -> Result<Vec<NetworkQuotaReport>> {
        desired.validate()?;
        let inner = &self.0.inner;
        let mut endpoints = Vec::new();
        let owners = desired
            .assignments
            .iter()
            .flat_map(|a| a.instances.iter())
            .collect::<std::collections::BTreeSet<_>>();
        let response = inner
            .http
            .get(&inner.cfg.network_inspection_url)
            .send()
            .map_err(|_| "Cube primary TAP inventory unavailable")?;
        if !response.status().is_success() {
            return Err("Cube primary TAP inventory rejected".into());
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        response
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cube primary TAP inventory failed")?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("Cube primary TAP inventory exceeds bound".into());
        }
        let taps: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "invalid Cube primary TAP inventory")?;
        for tap in array(&taps, "taps") {
            let id = text(tap, "sandboxID");
            if owners.iter().any(|owner| owner.as_str() == id) && number(tap, "tapIfIndex") > 0 {
                endpoints.push(json!({"ifindex":number(tap,"tapIfIndex"),"instance_id":id,"interface_id":"contact"}));
            }
        }
        let result = inner.network_rpc(
            "quota",
            "",
            Some(json!({
            "assignments":desired.assignments,"endpoints":endpoints})),
        )?;
        serde_json::from_value(result["reports"].clone())
            .map_err(|_| "invalid Cube network quota observation".into())
    }
}
