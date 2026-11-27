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
                            (Duration::from_secs(15), Duration::from_secs(15))
                        } else {
                            (
                                Duration::from_secs(15),
                                super::idle_poll_interval(&options, "reconcile"),
                            )
                        }
                    }
                    Err(error) => {
                        if error != previous_error {
                            eprintln!("cvm: {error}");
                            previous_error = error;
                        }
                        failures = failures.saturating_add(1);
                        let retry = Duration::from_secs(
                            (5u64.saturating_mul(1u64 << failures.min(5))).min(120),
                        );
                        (retry, retry)
                    }
                };
                wake.wait_connected(interval.0, interval.1);
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
        terminal_sessions_used: None,
    };
    if options.apply_mode != ApplyMode::Linux {
        inventory.error = Some("CVM 执行需要 Linux 收敛模式".into());
    } else {
        match backend.as_ref() {
            Ok(service) => match service.inventory() {
                Ok(capabilities) => {
                    #[cfg(target_os = "linux")]
                    if capabilities.guest_terminal && capabilities.guest_terminal_persistent {
                        inventory.terminal_sessions_used = cube_terminal::server::occupied_slots(
                            std::path::Path::new(cube_terminal::ROOT),
                        )
                        .ok()
                        .and_then(|used| u8::try_from(used).ok());
                    }
                    inventory.capabilities = capabilities;
                }
                Err(error) => inventory.error = Some(error),
            },
            Err(error) if configured => inventory.error = Some(error.clone()),
            Err(_) => {}
        }
    }
    let response = client.request(
        "POST",
        "/agent/v1/cvm/inventory",
        &options.token,
        Some(&serde_json::to_string(&inventory).map_err(|_| "CVM inventory encoding failed")?),
    )?;
    if ![200, 204].contains(&response.status) {
        return Err(format!(
            "CVM inventory request failed: HTTP {}",
            response.status
        ));
    }
    if response.status == 200 && inventory.error.is_none() {
        let reply: serde_json::Value = serde_json::from_str(&response.body)
            .map_err(|_| "CVM manifest negotiation is invalid")?;
        let missing: Vec<String> = serde_json::from_value(reply["missing_image_manifests"].clone())
            .map_err(|_| "CVM manifest requests invalid")?;
        if missing.len() > MAX_CATALOG_ITEMS || missing.iter().any(|sha| !valid_digest(sha)) {
            return Err("CVM manifest requests exceed limits".into());
        }
        if let Ok(service) = &backend {
            for manifest in service
                .image_manifests()
                .into_iter()
                .filter(|m| missing.contains(&m.sha256))
            {
                post(&client, options, "/agent/v1/cvm/image-manifests", &manifest)?;
            }
        }
    }
    if options.apply_mode == ApplyMode::Linux && super::maintenance()? {
        return Ok(false);
    }
    let quota_error = if inventory.error.is_none() && inventory.capabilities.network_quota.is_some()
    {
        // Assign future instance identities before a newly created TAP can pass data.
        super::network_quota::round(options).err()
    } else {
        None
    };
    let response = client.request(
        "GET",
        &format!("/agent/v1/cvm/desired?offset={cursor}"),
        &options.token,
        None,
    )?;
    if response.status != 200 {
        return Err(format!(
            "CVM desired request failed: HTTP {}",
            response.status
        ));
    }
    let desired: DesiredState =
        serde_json::from_str(&response.body).map_err(|_| "CVM desired response is invalid")?;
    if desired.page_offset != *cursor || desired.instances.len() > 8 {
        return Err("CVM desired page does not match its cursor or size limit".into());
    }
    if options.apply_mode == ApplyMode::Linux {
        super::chain_egress::sync(options, &desired.chain_clients)?;
    }
    if desired.instances.is_empty() {
        *cursor = 0;
        return Ok(false);
    }
    for item in &desired.instances {
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
        if item.power == PowerState::Running && item.spec.login_revision > 0 {
            let response = client.request(
                "GET",
                &format!(
                    "/agent/v1/cvm/{}/credential?revision={}",
                    item.id, item.spec.login_revision
                ),
                &options.token,
                None,
            )?;
            if response.status != 200 {
                return Err(format!(
                    "CVM credential request failed: HTTP {}",
                    response.status
                ));
            }
            if response.body.len() > 4096 {
                return Err("Guest credential response exceeds limit".into());
            }
            let credential: GuestCredential = serde_json::from_str(&response.body)
                .map_err(|_| "Guest credential response invalid")?;
            credential.validate()?;
            if credential.instance_id != item.id || credential.revision != item.spec.login_revision
            {
                return Err("Guest credential identity mismatch".into());
            }
            backend
                .as_ref()
                .map_err(Clone::clone)?
                .stage_credential(&credential)?;
        }
        // Readiness is an allocation gate. Cleanup still needs the owned executor
        // when a bridge or an unrelated capability observation is unavailable.
        let mut report = if options.apply_mode != ApplyMode::Linux
            || (inventory.error.is_some() && item.power != PowerState::Deleted)
            || (quota_error.is_some() && item.power == PowerState::Running)
            || (item.power == PowerState::Running
                && !super::chain_egress::ready(&item.id, &item.spec.routing))
        {
            let chain_error = super::chain_egress::receipts(&item.id)
                .into_iter()
                .find_map(|receipt| {
                    receipt
                        .error
                        .map(|error| format!("线路 {}：{error}", receipt.chain_id))
                });
            failed_report(
                item,
                ApplyStatus::Unknown,
                chain_error
                    .as_deref()
                    .unwrap_or("本机执行栈不可用；实例与预约保留"),
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
                Err(error) => {
                    let redactions =
                        super::diagnostics::secrets(&[
                            serde_json::to_string(&item.spec).unwrap_or_default()
                        ]);
                    super::diagnostics::record(
                        &options.state_dir,
                        Some(&item.id),
                        NetworkLogSource::Agent,
                        NetworkLogLevel::Error,
                        &format!(
                            "实例配置提交失败：{}",
                            super::diagnostics::redact(&error, &redactions)
                        ),
                    );
                    failed_report(
                        item,
                        ApplyStatus::Unknown,
                        "Cube 执行通信中断；先核查原运行时，不重复创建",
                    )
                }
            }
        };
        report.chain_egress = super::chain_egress::receipts(&item.id);
        if report.status == ApplyStatus::Applied {
            super::chain_egress::retire(options, item)?;
            report.chain_egress = super::chain_egress::receipts(&item.id);
        }
        super::diagnostics::instance(&options.state_dir, item, &report);
        post(&client, options, "/agent/v1/cvm/report", &report)?;
    }
    *cursor = desired.next_offset.unwrap_or(0);
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
