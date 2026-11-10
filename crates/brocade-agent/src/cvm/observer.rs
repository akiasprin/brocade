//! Metrics remain available when desired-state application or inventory is failing.
use super::reconcile::post;
use crate::{
    http::HttpClient,
    options::{ApplyMode, Options},
};
use brocade_deployment::cvm::*;
use std::time::Instant;
use std::{thread, time::Duration};

pub(super) fn spawn(options: Options) -> Result<(), String> {
    thread::Builder::new()
        .name("cvm-observer".into())
        .spawn(move || {
            let mut previous_error = String::new();
            loop {
                let started = Instant::now();
                let wait = match round(&options) {
                    Ok(active) => {
                        previous_error.clear();
                        if active {
                            15
                        } else {
                            60
                        }
                    }
                    Err(error) => {
                        if error != previous_error {
                            eprintln!("cvm observation: {error}");
                            previous_error = error;
                        }
                        30
                    }
                };
                thread::sleep(Duration::from_secs(wait).saturating_sub(started.elapsed()));
            }
        })
        .map_err(|e| format!("cannot spawn CVM observer: {e}"))?;
    Ok(())
}
#[derive(serde::Deserialize)]
struct Targets {
    targets: Vec<ObservationTarget>,
}
pub(super) fn round(options: &Options) -> Result<bool, String> {
    if options.apply_mode != ApplyMode::Linux {
        return Ok(false);
    }
    let client = HttpClient::new(&options.server)?;
    let response = client.request(
        "GET",
        "/agent/v1/cvm/observation-targets",
        &options.token,
        None,
    )?;
    if response.status == 404 {
        return Ok(false);
    }
    if response.status != 200 {
        return Err(format!("CVM observation targets: HTTP {}", response.status));
    }
    let targets: Targets =
        serde_json::from_str(&response.body).map_err(|_| "invalid CVM observation targets")?;
    if targets.targets.len() > MAX_INSTANCES_PER_MACHINE
        || targets
            .targets
            .iter()
            .any(|t| !valid_id(&t.id) || t.generation == 0 || t.services.len() > 8)
    {
        return Err("CVM observation targets exceed limits".into());
    }
    if targets.targets.is_empty() {
        return Ok(false);
    }
    let service = super::backend::get()?;
    let ids = targets
        .targets
        .iter()
        .map(|t| t.id.clone())
        .collect::<Vec<_>>();
    // These are cached observations. No Guest command runs in this request path.
    // Paging limits each message; every assigned instance is visited each round.
    for chunk in targets.targets.chunks(8) {
        let mut observations = service.observe(chunk, &ids)?;
        for observed in &mut observations {
            let Some(target) = chunk.iter().find(|target| target.id == observed.id) else {
                continue;
            };
            for probe in &target.services {
                let mut result =
                    super::nics::probe_service(&probe.interface_id, &probe.host, probe.port);
                // Entry services are probed at the primary NIC address from this host.
                if let Ok(None) = result {
                    result = super::entries::probe(&probe.host, probe.port);
                }
                if let Ok(None) = result {
                    continue;
                }
                let (state, latency_millis) = match result {
                    Ok(Some(value)) => value,
                    Err(error) => {
                        observed.error = Some(format!("NIC service observation: {error}"));
                        (Reachability::Unknown, None)
                    }
                    Ok(None) => unreachable!("unmanaged service probes continue above"),
                };
                if let Some(service) = observed
                    .services
                    .iter_mut()
                    .find(|service| service.id == probe.id)
                {
                    service.state = state;
                    service.latency_millis = latency_millis;
                }
            }
        }
        if observations.len() > 32
            || observations.iter().any(|o| {
                !chunk
                    .iter()
                    .any(|t| o.id == t.id && o.generation == t.generation)
            })
        {
            return Err("CVM observation response is outside its assignment".into());
        }
        if !observations.is_empty() {
            post(
                &client,
                options,
                "/agent/v1/cvm/observations",
                &serde_json::json!({"observations":observations}),
            )?;
        }
    }
    Ok(true)
}
