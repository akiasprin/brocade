//! Independently sampled Guest observations and bounded minute history.
use super::*;
mod diagnostics;
pub use diagnostics::DiagnosticReading;
mod observations;
pub use observations::{MonitoringEvent, MonitoringIssue};

const HISTORY_MILLIS: u64 = 86_400_000;
const MAX_RATE_INTERVAL: u64 = 120_000;

pub(super) fn now_millis() -> Result<u64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| invalid("system clock is before the Unix epoch"))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| invalid("system clock exceeds the supported range"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceRates {
    #[serde(default)]
    pub rx_pps: Option<f64>,
    #[serde(default)]
    pub tx_pps: Option<f64>,
    #[serde(default)]
    pub drops: Option<u64>,
    #[serde(default)]
    pub errors: Option<u64>,
    #[serde(default)]
    pub carrier: Option<bool>,
    pub id: String,
    pub device: String,
    pub rx_bps: Option<u64>,
    pub tx_bps: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringSample {
    #[serde(default)]
    pub diagnostics: Option<DiagnosticReading>,
    #[serde(default)]
    pub cpu_peak_basis_points: Option<u32>,
    #[serde(default)]
    pub memory_available_min_bytes: Option<u64>,
    #[serde(default)]
    pub has_gap: bool,
    pub observed_at_unix_millis: u64,
    pub window_start_unix_millis: u64,
    pub uptime_millis: u64,
    pub cpu_usage_basis_points: Option<u32>,
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub root_total_bytes: u64,
    pub root_used_bytes: u64,
    pub interfaces: Vec<InterfaceRates>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmMonitoring {
    pub samples_truncated: bool,
    pub latest: Option<MonitoringSample>,
    pub health: Option<InstanceObservation>,
    pub issues: Vec<MonitoringIssue>,
    pub events: Vec<MonitoringEvent>,
    pub state: String,
    pub error: Option<String>,
    pub range_start_unix_millis: u64,
    pub range_end_unix_millis: u64,
    pub samples: Vec<MonitoringSample>,
}

fn sample(current: &GuestMetrics, previous: Option<&GuestMetrics>) -> MonitoringSample {
    let previous = previous.filter(|p| {
        p.boot_id == current.boot_id
            && current.uptime_millis > p.uptime_millis
            && current.uptime_millis - p.uptime_millis <= MAX_RATE_INTERVAL
            && current.observed_at_unix_millis > p.observed_at_unix_millis
            && current.observed_at_unix_millis - p.observed_at_unix_millis <= MAX_RATE_INTERVAL
    });
    let cpu = previous.and_then(|p| {
        let total = current.cpu_total_ticks.checked_sub(p.cpu_total_ticks)?;
        let busy = current.cpu_busy_ticks.checked_sub(p.cpu_busy_ticks)?;
        if total == 0 || busy > total {
            return None;
        }
        Some((u128::from(busy) * 10_000 / u128::from(total)) as u32)
    });
    let rate = |value: u64, before: u64, interval: u64| -> Option<u64> {
        let bits = u128::from(value.checked_sub(before)?) * 8 * 1000 / u128::from(interval);
        (bits < (1 << 53)).then_some(bits as u64)
    };
    MonitoringSample {
        diagnostics: diagnostics::diagnostics(current, previous),
        cpu_peak_basis_points: cpu,
        memory_available_min_bytes: Some(current.memory_available_bytes),
        has_gap: previous.is_none(),
        observed_at_unix_millis: current.observed_at_unix_millis,
        window_start_unix_millis: previous.map_or(current.observed_at_unix_millis, |p| {
            p.observed_at_unix_millis
        }),
        uptime_millis: current.uptime_millis,
        cpu_usage_basis_points: cpu,
        memory_total_bytes: current.memory_total_bytes,
        memory_used_bytes: current.memory_total_bytes - current.memory_available_bytes,
        root_total_bytes: current.root_total_bytes,
        root_used_bytes: current.root_used_bytes,
        interfaces: current
            .interfaces
            .iter()
            .map(|n| {
                let prior = previous.and_then(|p| {
                    p.interfaces.iter().find(|old| {
                        old.id == n.id && old.device == n.device && old.ifindex == n.ifindex
                    })
                });
                let interval = previous.map_or(1, |p| current.uptime_millis - p.uptime_millis);
                let count_delta =
                    |a: Option<u64>, b: Option<u64>| a.zip(b).and_then(|(a, b)| a.checked_sub(b));
                InterfaceRates {
                    rx_pps: prior
                        .and_then(|p| count_delta(n.rx_packets, p.rx_packets))
                        .map(|v| v as f64 * 1000.0 / interval as f64),
                    tx_pps: prior
                        .and_then(|p| count_delta(n.tx_packets, p.tx_packets))
                        .map(|v| v as f64 * 1000.0 / interval as f64),
                    drops: prior
                        .and_then(|p| {
                            count_delta(n.rx_dropped, p.rx_dropped)
                                .zip(count_delta(n.tx_dropped, p.tx_dropped))
                        })
                        .map(|(a, b)| a + b),
                    errors: prior
                        .and_then(|p| {
                            count_delta(n.rx_errors, p.rx_errors)
                                .zip(count_delta(n.tx_errors, p.tx_errors))
                        })
                        .map(|(a, b)| a + b),
                    carrier: n.carrier,
                    id: n.id.clone(),
                    device: n.device.clone(),
                    rx_bps: prior.and_then(|p| rate(n.rx_bytes, p.rx_bytes, interval)),
                    tx_bps: prior.and_then(|p| rate(n.tx_bytes, p.tx_bytes, interval)),
                }
            })
            .collect(),
    }
}

impl PgStore {
    pub async fn cvm_monitoring(&self, actor: &AdminContext, id: &str) -> Result<CvmMonitoring> {
        self.cvm_monitoring_range(actor, id, 3600).await
    }
    pub async fn cvm_monitoring_range(
        &self,
        actor: &AdminContext,
        id: &str,
        seconds: u32,
    ) -> Result<CvmMonitoring> {
        if ![3600, 21600, 86400].contains(&seconds) {
            return Err(invalid("unsupported CVM monitoring range"));
        }
        let row = sqlx::query("SELECT tenant_id,user_id FROM cvm_instances WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::NotFound("CVM instance".into()))?;
        require_owner(
            actor,
            &row.try_get::<String, _>("tenant_id")?,
            &row.try_get::<String, _>("user_id")?,
            false,
        )?;
        let now = now_millis()?;
        let state = sqlx::query(
            "SELECT observation,latest_sample FROM cvm_observation_state WHERE instance_id=$1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let (mut health, latest, error) = if let Some(state) = state {
            let h: InstanceObservation = serde_json::from_value(state.try_get("observation")?)?;
            let latest: Option<MonitoringSample> = state
                .try_get::<Option<serde_json::Value>, _>("latest_sample")?
                .map(serde_json::from_value)
                .transpose()?;
            let error = h.error.clone();
            (Some(h), latest, error)
        } else {
            (None, None, None)
        };
        if actor.role() == AdminRole::Readonly {
            if let Some(health) = health.as_mut() {
                health.runtime_id = None;
            }
        }
        // Accepted observations permit bounded node clock skew. Preserve their
        // actual timestamps and include them in the selected, equally sized range.
        let end = now
            .max(latest.as_ref().map_or(0, |s| s.observed_at_unix_millis))
            .max(health.as_ref().map_or(0, |h| h.observed_at_unix_millis));
        let start = end.saturating_sub(u64::from(seconds) * 1000);
        let rows:Vec<serde_json::Value>=sqlx::query_scalar("SELECT sample FROM cvm_observation_samples WHERE instance_id=$1 AND observed_at>=$2 AND observed_at<=$3 ORDER BY observed_at DESC LIMIT 2001").bind(id).bind(start as i64).bind(end as i64).fetch_all(&self.pool).await?;
        let truncated = rows.len() > 2000;
        let mut samples = rows
            .into_iter()
            .take(2000)
            .map(serde_json::from_value::<MonitoringSample>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        samples.reverse();
        let running = health
            .as_ref()
            .is_some_and(|h| h.runtime_state == ObservedRuntimeState::Running);
        let stopped = health
            .as_ref()
            .is_some_and(|h| h.runtime_state == ObservedRuntimeState::Stopped);
        let fresh = latest
            .as_ref()
            .is_some_and(|s| now.saturating_sub(s.observed_at_unix_millis) <= MAX_RATE_INTERVAL)
            && running
            && error.is_none();
        let label = if stopped {
            "stopped"
        } else if fresh {
            "live"
        } else if error.is_some() {
            "unavailable"
        } else if latest.is_some() {
            "stale"
        } else {
            "unavailable"
        };
        let mut issues = observations::issues(latest.as_ref(), health.as_ref(), fresh);
        if end > now.saturating_add(5000) {
            issues.push(MonitoringIssue {
                kind: "clock".into(),
                severity: "warn".into(),
                message: format!(
                    "承载机器时钟领先 Console 约 {} 秒；历史保留上报时间，请校准机器时钟",
                    (end - now) / 1000
                ),
            });
        }
        let events=sqlx::query("SELECT observed_at,kind,detail FROM cvm_observation_events WHERE instance_id=$1 AND observed_at>=$2 UNION ALL SELECT (extract(epoch FROM created_at)*1000)::bigint AS observed_at,kind,('实例操作 · '||status) AS detail FROM cvm_operations WHERE instance_id=$1 AND created_at>=to_timestamp($2::double precision/1000) ORDER BY observed_at DESC LIMIT 100").bind(id).bind(start as i64).fetch_all(&self.pool).await?.into_iter().map(|r|Ok(MonitoringEvent{at_unix_millis:r.try_get::<i64,_>("observed_at")? as u64,kind:r.try_get("kind")?,detail:r.try_get("detail")?})).collect::<Result<Vec<_>>>()?;
        Ok(CvmMonitoring {
            samples_truncated: truncated,
            latest,
            health,
            issues,
            events,
            state: label.into(),
            error,
            range_start_unix_millis: start,
            range_end_unix_millis: end,
            samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn counters() -> GuestMetrics {
        GuestMetrics {
            diagnostics: None,
            observed_at_unix_millis: 200_000,
            boot_id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            uptime_millis: 100_000,
            cpu_total_ticks: 1000,
            cpu_busy_ticks: 200,
            memory_total_bytes: 100,
            memory_available_bytes: 30,
            root_total_bytes: 200,
            root_used_bytes: 80,
            interfaces: vec![InterfaceCounters {
                id: "nic-1".into(),
                device: "eth0".into(),
                ifindex: 2,
                rx_bytes: 1000,
                tx_bytes: 2000,
                ..Default::default()
            }],
        }
    }
    #[test]
    fn counter_resets_device_replacement_and_long_gaps_do_not_fabricate_rates() {
        let old = counters();
        let mut new = old.clone();
        new.observed_at_unix_millis += 30_000;
        new.uptime_millis += 30_000;
        new.cpu_total_ticks += 100;
        new.cpu_busy_ticks += 25;
        new.interfaces[0].rx_bytes += 30_000;
        assert_eq!(sample(&new, Some(&old)).cpu_usage_basis_points, Some(2500));
        assert_eq!(sample(&new, Some(&old)).interfaces[0].rx_bps, Some(8000));
        new.interfaces[0].ifindex = 3;
        assert_eq!(sample(&new, Some(&old)).interfaces[0].rx_bps, None);
        new.interfaces[0].ifindex = 2;
        new.interfaces[0].rx_bytes = 1;
        assert_eq!(sample(&new, Some(&old)).interfaces[0].rx_bps, None);
        new.cpu_busy_ticks = old.cpu_busy_ticks - 1;
        assert_eq!(sample(&new, Some(&old)).cpu_usage_basis_points, None);
        new.observed_at_unix_millis += MAX_RATE_INTERVAL;
        new.uptime_millis += MAX_RATE_INTERVAL;
        let gap = sample(&new, Some(&old));
        assert_eq!(gap.cpu_usage_basis_points, None);
        assert_eq!(gap.window_start_unix_millis, new.observed_at_unix_millis);
    }
}
