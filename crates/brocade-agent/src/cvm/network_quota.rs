//! Quota authority is cached before execution; a failed receipt never frees Store capacity.
use crate::{
    fsutil::{atomic_write_private, read_bounded},
    http::HttpClient,
    options::{ApplyMode, Options},
};
use brocade_deployment::cvm::*;
use std::{sync::Mutex, time::Duration};

static ROUND: Mutex<()> = Mutex::new(());

pub(super) fn spawn(options: Options) -> Result<(), String> {
    std::thread::Builder::new()
        .name("cvm-network-quota".into())
        .spawn(move || {
            let mut previous = String::new();
            loop {
                match round(&options) {
                    Ok(()) => previous.clear(),
                    Err(error) => {
                        if error != previous {
                            eprintln!("cvm network quota: {error}");
                            previous = error;
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        })
        .map_err(|_| "cannot spawn network quota convergence")?;
    Ok(())
}

pub(super) fn round(options: &Options) -> Result<(), String> {
    let _guard = ROUND
        .lock()
        .map_err(|_| "network quota convergence lock unavailable")?;
    if options.apply_mode != ApplyMode::Linux || super::maintenance()? {
        return Ok(());
    }
    if !super::backend::configured()? {
        return Ok(());
    }
    let backend = super::backend::get()?;
    if !backend.supports_network_quotas() {
        return Ok(());
    }
    let client = HttpClient::new(&options.server)?;
    let path = options.state_dir.join("cvm-network-quotas.json");
    let response = client.request(
        "GET",
        "/agent/v1/cvm/network-quotas/desired",
        &options.token,
        None,
    );
    let desired = match response {
        Ok(response) if response.status == 200 => {
            if response.body.len() > 128 * 1024 {
                return Err("network quota desired exceeds bound".into());
            }
            let desired: NetworkQuotaDesired = serde_json::from_str(&response.body)
                .map_err(|_| "invalid network quota desired")?;
            desired.validate()?;
            if read_bounded(&path, 128 * 1024)?.as_deref() != Some(response.body.as_bytes()) {
                atomic_write_private(&path, response.body.as_bytes())?;
            }
            desired
        }
        error => {
            if let Some(saved) = read_bounded(&path, 128 * 1024)? {
                let desired: NetworkQuotaDesired = serde_json::from_slice(&saved)
                    .map_err(|_| "invalid persisted network quota authority")?;
                desired.validate()?;
                backend.apply_network_quotas(&desired)?;
            }
            return Err(match error {
                Ok(r) => format!("network quota desired: HTTP {}", r.status),
                Err(e) => e,
            });
        }
    };
    let reports = match backend.apply_network_quotas(&desired) {
        Ok(reports) => reports,
        Err(error) => desired
            .assignments
            .iter()
            .map(|a| NetworkQuotaReport {
                id: a.id.clone(),
                revision: a.revision,
                applied: false,
                states: 0,
                denied_capacity: 0,
                denied_rate: 0,
                allocation_failures: 0,
                error: Some(error.chars().take(120).collect()),
            })
            .collect(),
    };
    super::reconcile::post(
        &client,
        options,
        "/agent/v1/cvm/network-quotas/report",
        &serde_json::json!({"reports":reports}),
    )
}
