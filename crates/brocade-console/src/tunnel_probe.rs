//! Durable Console-originated external-tunnel probes.
//!
//! PostgreSQL owns scheduling and fencing. This module owns only the local executor: it starts a
//! short-lived Xray, renews the row lease while that process runs, and writes a bounded result.

use std::{sync::Arc, time::Duration};

use brocade_core::model::ExternalOutbound;
use brocade_probe::{
    ProbeCancellation, ProbeOptions, TunnelProbePhase as RuntimePhase, TunnelProbeResult,
    TunnelProbeStatus,
};
use brocade_store::{
    ClaimedTunnelProbe, PgStore, TunnelProbeCompletion, TunnelProbeJobStatus, TunnelProbePhase,
    TunnelProbeResultStatus, TUNNEL_PROBE_RETENTION_DAYS,
};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, Semaphore};

const SCHEDULER_TICK: Duration = Duration::from_secs(1);
const LEASE_REFRESH: Duration = Duration::from_secs(5);
const MAINTENANCE_TICK: Duration = Duration::from_secs(5);
const LOCAL_CONCURRENCY: usize = 4;
const PRUNE_EVERY_SCHEDULER_TICKS: u64 = 60;
const PRUNE_EVERY_MAINTENANCE_TICKS: u64 = 12;

/// Start the scheduler on this Console process.
///
/// A missing runtime disables execution but not the HTTP surface. Operators can therefore see the
/// capability reason, and queued durable work remains available after the runtime is repaired.
pub fn spawn(store: PgStore) {
    let runtime = crate::grant_probe::GrantProbeService::from_env();
    let capability = runtime.capability();
    if !capability.available {
        eprintln!(
            "tunnel probe worker disabled: {}",
            capability
                .reason
                .as_deref()
                .unwrap_or("Console probe runtime is unavailable")
        );
        spawn_maintenance(store);
        return;
    }

    let xray_binary = runtime.xray_binary();
    let runtime_dir = runtime.runtime_dir();
    let owner = format!("console-{}", std::process::id());
    let local = Arc::new(Semaphore::new(LOCAL_CONCURRENCY));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SCHEDULER_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick_count = 0_u64;
        let mut enqueue_failures = 0_u32;
        loop {
            tick.tick().await;
            tick_count = tick_count.wrapping_add(1);
            if let Err(error) = store.enqueue_due_tunnel_probes().await {
                if enqueue_failures == 0 || enqueue_failures.is_power_of_two() {
                    eprintln!("tunnel probe: enqueue failed: {error}");
                }
                enqueue_failures = enqueue_failures.saturating_add(1);
                let backoff_secs = 1_u64 << enqueue_failures.min(5);
                // A bad due policy must not starve already queued manual work. Claiming is safe
                // to try independently; a real database outage simply makes this attempt fail
                // too and the bounded scheduler backoff still applies.
                dispatch_available(&store, &owner, &local, &xray_binary, &runtime_dir).await;
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                continue;
            }
            if enqueue_failures > 0 {
                eprintln!("tunnel probe: scheduler database access recovered");
                enqueue_failures = 0;
            }
            dispatch_available(&store, &owner, &local, &xray_binary, &runtime_dir).await;
            if tick_count.is_multiple_of(PRUNE_EVERY_SCHEDULER_TICKS) {
                match store.prune_tunnel_probes(TUNNEL_PROBE_RETENTION_DAYS).await {
                    Ok(0) => {}
                    Ok(rows) => eprintln!("tunnel probe: pruned {rows} expired runs"),
                    Err(error) => eprintln!("tunnel probe: prune failed: {error}"),
                }
            }
        }
    });
}

fn spawn_maintenance(store: PgStore) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(MAINTENANCE_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick_count = 0_u64;
        let mut reap_failed = false;
        loop {
            tick.tick().await;
            tick_count = tick_count.wrapping_add(1);
            match store.reap_expired_tunnel_probes().await {
                Ok(rows) => {
                    if reap_failed {
                        eprintln!("tunnel probe: expired-run cleanup recovered");
                        reap_failed = false;
                    }
                    if rows > 0 {
                        eprintln!("tunnel probe: marked {rows} expired runs interrupted");
                    }
                }
                Err(error) => {
                    if !reap_failed {
                        eprintln!("tunnel probe: expired-run cleanup failed: {error}");
                    }
                    reap_failed = true;
                }
            }
            if tick_count.is_multiple_of(PRUNE_EVERY_MAINTENANCE_TICKS) {
                if let Err(error) = store.prune_tunnel_probes(TUNNEL_PROBE_RETENTION_DAYS).await {
                    eprintln!("tunnel probe: prune failed: {error}");
                }
            }
        }
    });
}

async fn dispatch_available(
    store: &PgStore,
    owner: &str,
    local: &Arc<Semaphore>,
    xray_binary: &std::path::Path,
    runtime_dir: &std::path::Path,
) {
    loop {
        let Ok(local_permit) = local.clone().try_acquire_owned() else {
            return;
        };
        let Ok(global_permit) = crate::grant_probe::global_probe_semaphore().try_acquire_owned()
        else {
            return;
        };
        let claim = match store.claim_next_tunnel_probe(owner).await {
            Ok(Some(claim)) => claim,
            Ok(None) => return,
            Err(error) => {
                eprintln!("tunnel probe: claim failed: {error}");
                return;
            }
        };
        let store = store.clone();
        let xray_binary = xray_binary.to_path_buf();
        let runtime_dir = runtime_dir.to_path_buf();
        tokio::spawn(async move {
            let _local_permit = local_permit;
            let _global_permit = global_permit;
            execute_claim(store, claim, xray_binary, runtime_dir).await;
        });
    }
}

async fn execute_claim(
    store: PgStore,
    claim: ClaimedTunnelProbe,
    xray_binary: std::path::PathBuf,
    runtime_dir: std::path::PathBuf,
) {
    let outbound = match store.claimed_tunnel_probe_outbound(&claim).await {
        Ok(outbound) => outbound,
        Err(error) => {
            let _ = store
                .fail_tunnel_probe_claim(
                    &claim,
                    "frozen-config-unavailable",
                    "拨测冻结的发布配置已不可用",
                )
                .await;
            eprintln!(
                "tunnel probe {}: load frozen config failed: {error}",
                claim.run.id
            );
            return;
        }
    };
    let fingerprint = match redacted_fingerprint(&outbound) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            let _ = store
                .fail_tunnel_probe_claim(&claim, "config-fingerprint-failed", "无法冻结拨测配置")
                .await;
            eprintln!(
                "tunnel probe {}: config fingerprint failed: {error}",
                claim.run.id
            );
            return;
        }
    };
    match store
        .set_tunnel_probe_config_sha256(&claim, &fingerprint)
        .await
    {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            eprintln!(
                "tunnel probe {}: persist config fingerprint failed: {error}",
                claim.run.id
            );
            return;
        }
    }

    let cancellation = ProbeCancellation::default();
    let options = ProbeOptions::new(xray_binary, cancellation.clone())
        .with_runtime_dir(runtime_dir)
        .without_warm_up();
    let endpoint_url = claim.endpoint_url.clone();
    let timeout_secs = u64::from(claim.run.timeout_secs);
    let (phase_tx, mut phase_rx) = mpsc::unbounded_channel();
    let mut task = tokio::task::spawn_blocking(move || {
        brocade_probe::probe_external_outbound_with_options(
            &outbound,
            &endpoint_url,
            timeout_secs,
            &options,
            |phase| {
                let _ = phase_tx.send(phase);
            },
        )
    });
    let mut refresh =
        tokio::time::interval_at(tokio::time::Instant::now() + LEASE_REFRESH, LEASE_REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let joined = loop {
        tokio::select! {
            result = &mut task => break result,
            Some(phase) = phase_rx.recv() => {
                let phase = match phase {
                    RuntimePhase::StartingXray => TunnelProbePhase::StartingXray,
                    RuntimePhase::Requesting => TunnelProbePhase::Requesting,
                };
                match store.update_tunnel_probe_phase(&claim, phase).await {
                    Ok(true) => {}
                    Ok(false) => cancellation.cancel(),
                    Err(error) => eprintln!("tunnel probe {}: phase update failed: {error}", claim.run.id),
                }
            }
            _ = refresh.tick() => {
                match store.renew_tunnel_probe_lease(&claim).await {
                    Ok(true) => {}
                    Ok(false) => cancellation.cancel(),
                    Err(error) => {
                        eprintln!("tunnel probe {}: lease refresh failed: {error}", claim.run.id);
                        // Continuing after an uncertain renewal can overlap the next
                        // claimant once this lease expires. Stop this exact Xray child;
                        // the fenced completion write will be ignored if ownership has
                        // already moved on.
                        cancellation.cancel();
                    }
                }
                match store.tunnel_probe_cancel_requested(&claim).await {
                    Ok(true) => cancellation.cancel(),
                    Ok(false) => {}
                    Err(error) => eprintln!("tunnel probe {}: cancel check failed: {error}", claim.run.id),
                }
            }
        }
    };

    let completion = match joined {
        Ok(result) => completion_from_result(result),
        Err(_) => TunnelProbeCompletion {
            status: TunnelProbeJobStatus::Failed,
            result: TunnelProbeResultStatus::Interrupted,
            ttfb_ms: None,
            http_status: None,
            exit_ip: None,
            exit_loc: None,
            attempt_count: 0,
            error_code: Some("worker-interrupted".to_owned()),
            error_detail: Some("拨测执行线程异常结束".to_owned()),
        },
    };
    if let Err(error) = store.complete_tunnel_probe(&claim, completion).await {
        eprintln!(
            "tunnel probe {}: completion write failed: {error}",
            claim.run.id
        );
    }
}

fn completion_from_result(result: TunnelProbeResult) -> TunnelProbeCompletion {
    let (status, stored_result, code, detail) = match result.status {
        TunnelProbeStatus::Ok => (
            TunnelProbeJobStatus::Succeeded,
            TunnelProbeResultStatus::Ok,
            None,
            None,
        ),
        TunnelProbeStatus::Timeout => (
            TunnelProbeJobStatus::Failed,
            TunnelProbeResultStatus::Timeout,
            Some("timeout".to_owned()),
            Some("公共落点在时限内没有返回".to_owned()),
        ),
        TunnelProbeStatus::ConnectFailed => (
            TunnelProbeJobStatus::Failed,
            TunnelProbeResultStatus::ConnectFailed,
            Some("connect-failed".to_owned()),
            Some("隧道未能连接到公共落点".to_owned()),
        ),
        TunnelProbeStatus::TargetFailed => (
            TunnelProbeJobStatus::Failed,
            TunnelProbeResultStatus::TargetFailed,
            Some("target-failed".to_owned()),
            Some("公共落点返回了失败状态".to_owned()),
        ),
        TunnelProbeStatus::Unsupported => (
            TunnelProbeJobStatus::Unsupported,
            TunnelProbeResultStatus::Unsupported,
            Some("unsupported".to_owned()),
            Some("Console 无法执行该拨测配置".to_owned()),
        ),
        TunnelProbeStatus::Canceled => (
            TunnelProbeJobStatus::Canceled,
            TunnelProbeResultStatus::Canceled,
            Some("canceled".to_owned()),
            Some("操作者取消了拨测".to_owned()),
        ),
    };
    TunnelProbeCompletion {
        status,
        result: stored_result,
        ttfb_ms: result.ttfb_ms,
        http_status: result.http_status,
        exit_ip: result.exit_ip,
        exit_loc: result.exit_loc,
        attempt_count: result.attempts,
        error_code: code,
        // Never persist the raw Xray detail: it may contain upstream addresses or account data.
        error_detail: detail,
    }
}

fn redacted_fingerprint(outbound: &ExternalOutbound) -> Result<String, serde_json::Error> {
    let mut redacted = outbound.clone();
    redacted.protocol.set_credential("<redacted>".to_owned());
    redacted.bindings.clear();
    let encoded = serde_json::to_vec(&redacted)?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_never_keeps_raw_runtime_detail() {
        let completion = completion_from_result(TunnelProbeResult {
            status: TunnelProbeStatus::ConnectFailed,
            ttfb_ms: None,
            http_status: None,
            exit_ip: None,
            exit_loc: None,
            attempts: 2,
            detail: Some("secret credential and upstream address".to_owned()),
        });
        assert_eq!(completion.error_code.as_deref(), Some("connect-failed"));
        assert_eq!(completion.attempt_count, 2);
        assert!(!completion.error_detail.unwrap().contains("secret"));
    }
}
