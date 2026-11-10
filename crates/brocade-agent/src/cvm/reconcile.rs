//! Persist immutable intent before submitting it to the in-process Cube executor.
use crate::{
    http::HttpClient,
    options::{ApplyMode, Options},
};
use brocade_deployment::cvm::*;
use std::{thread, time::Duration};

pub(super) fn spawn(options: Options) -> Result<(), String> {
    thread::Builder::new()
        .name("cvm".into())
        .spawn(move || {
            let mut wake =
                crate::wake::Cursor::new(brocade_deployment::agent_channel::WakeDomain::Desired);
            let mut cursor = 0;
            let mut failures = 0u32;
            let mut previous_error = String::new();
            loop {
                let interval = match round(&options, &mut cursor) {
                    Ok(active) => {
                        failures = 0;
                        previous_error.clear();
                        if active {
                            Duration::from_secs(15)
                        } else {
                            Duration::from_secs(60)
                        }
                    }
                    Err(error) => {
                        if error != previous_error {
                            eprintln!("cvm: {error}");
                            previous_error = error;
                        }
                        failures = failures.saturating_add(1);
                        Duration::from_secs((5u64.saturating_mul(1u64 << failures.min(5))).min(120))
                    }
                };
                wake.wait(interval);
            }
        })
        .map_err(|e| format!("cannot spawn CVM worker: {e}"))?;
    Ok(())
}
pub(super) fn round(options: &Options, cursor: &mut usize) -> Result<bool, String> {
    let client = HttpClient::new(&options.server)?;
    let configured = super::backend::configured()?;
    let backend = super::backend::get();
    let mut inventory = InventoryReport {
        capabilities: Capabilities::default(),
        error: None,
    };
    if options.apply_mode != ApplyMode::Linux {
        inventory.error = Some("CVM 执行需要 Linux 收敛模式".into());
    } else {
        match backend.as_ref() {
            Ok(service) => match service.inventory() {
                Ok(capabilities) => inventory.capabilities = capabilities,
                Err(error) => inventory.error = Some(error),
            },
            Err(error) if configured => inventory.error = Some(error.clone()),
            Err(_) => {}
        }
    }
    post(&client, options, "/agent/v1/cvm/inventory", &inventory)?;
    if super::maintenance()? {
        return Ok(false);
    }
    let quota_error = if inventory.error.is_none() && inventory.capabilities.network_quota.is_some()
    {
        // Assign future instance identities before a newly created TAP can pass data.
        super::network_quota::round(options).err()
    } else {
        None
    };
    let response = client.request("GET", "/agent/v1/cvm/desired", &options.token, None)?;
    if response.status != 200 {
        return Err(format!(
            "CVM desired request failed: HTTP {}",
            response.status
        ));
    }
    let desired: DesiredState =
        serde_json::from_str(&response.body).map_err(|_| "CVM desired response is invalid")?;
    if desired.instances.len() > MAX_INSTANCES_PER_MACHINE {
        return Err("CVM desired set exceeds the machine limit".into());
    }
    if desired.instances.is_empty() {
        *cursor = 0;
        return Ok(false);
    }
    for offset in 0..desired.instances.len().min(8) {
        let item = &desired.instances[(*cursor + offset) % desired.instances.len()];
        if !valid_id(&item.id)
            || !valid_id(&item.operation_id)
            || item.generation == 0
            || item.spec_sha256
                != brocade_core::hash::sha256_hex(
                    &serde_json::to_vec(&item.spec)
                        .map_err(|_| "CVM specification serialization failed")?,
                )
        {
            return Err("CVM desired specification identity is invalid".into());
        }
        // Readiness is an allocation gate. Cleanup still needs the owned executor
        // when a bridge or an unrelated capability observation is unavailable.
        let report = if options.apply_mode != ApplyMode::Linux
            || (inventory.error.is_some() && item.power != PowerState::Deleted)
            || (quota_error.is_some() && item.power == PowerState::Running)
        {
            failed_report(
                item,
                ApplyStatus::Unknown,
                "本机 Cube 执行器不可用；实例与预约保留",
            )
        } else {
            match backend
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|service| service.submit(item))
            {
                Ok(report) => match report.validate_for(item) {
                    Ok(()) => report,
                    Err(_) => failed_report(
                        item,
                        ApplyStatus::Failed,
                        "执行回执缺少对应版本、设备或恢复证据",
                    ),
                },
                Err(_) => failed_report(
                    item,
                    ApplyStatus::Unknown,
                    "Cube 执行通信中断；先核查原运行时，不重复创建",
                ),
            }
        };
        post(&client, options, "/agent/v1/cvm/report", &report)?;
    }
    *cursor = (*cursor + 8) % desired.instances.len();
    Ok(true)
}
fn failed_report(desired: &InstanceDesired, status: ApplyStatus, error: &str) -> InstanceReport {
    brocade_cube::report(desired, status, Some(error))
}
pub(super) fn post<T: serde::Serialize>(
    client: &HttpClient,
    options: &Options,
    path: &str,
    value: &T,
) -> Result<(), String> {
    let body = serde_json::to_string(value).map_err(|_| "CVM request serialization failed")?;
    let response = client.request("POST", path, &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "CVM report request failed: HTTP {}",
            response.status
        ));
    }
    Ok(())
}
