//! Bounded fleet readings. Scope and list bounds match the workspace, without
//! issuing monitoring/history requests separately for every instance.
use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
pub struct CvmCardSample {
    pub observed_at_unix_millis: u64,
    pub window_start_unix_millis: u64,
    pub has_gap: bool,
    pub cpu_usage_basis_points: Option<u32>,
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub root_total_bytes: u64,
    pub root_used_bytes: u64,
    pub rx_bps: Option<u64>,
    pub tx_bps: Option<u64>,
}

impl From<MonitoringSample> for CvmCardSample {
    fn from(s: MonitoringSample) -> Self {
        let rate = |rx: bool| {
            if s.interfaces.is_empty() {
                return None;
            }
            s.interfaces.iter().try_fold(0_u64, |sum, nic| {
                sum.checked_add(if rx { nic.rx_bps? } else { nic.tx_bps? })
            })
        };
        Self {
            observed_at_unix_millis: s.observed_at_unix_millis,
            window_start_unix_millis: s.window_start_unix_millis,
            has_gap: s.has_gap,
            cpu_usage_basis_points: s.cpu_usage_basis_points,
            memory_total_bytes: s.memory_total_bytes,
            memory_used_bytes: s.memory_used_bytes,
            root_total_bytes: s.root_total_bytes,
            root_used_bytes: s.root_used_bytes,
            rx_bps: rate(true),
            tx_bps: rate(false),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CvmCardReading {
    pub id: String,
    pub state: &'static str,
    pub latest: Option<CvmCardSample>,
    pub samples: Vec<CvmCardSample>,
    pub month_used_bytes: Option<u64>,
    pub month_limit_bytes: Option<u64>,
    pub month_coverage_gap: bool,
    pub month_observed_at_unix_millis: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CvmReadings {
    pub observed_at_unix_millis: u64,
    pub instances: Vec<CvmCardReading>,
}

impl PgStore {
    pub async fn cvm_readings(&self, actor: &AdminContext) -> Result<CvmReadings> {
        let (tenant, pattern, user) = read_scope(actor)?;
        let now = monitoring::now_millis()?;
        let rows = sqlx::query(&format!(
            "WITH visible AS (SELECT id,desired FROM cvm_instances WHERE {READ_SCOPE}
             ORDER BY reserved DESC,created_at DESC,id LIMIT 512)
             SELECT v.id,v.desired,o.observation-'metrics'-'runtime_id' AS observation,
               o.latest_sample-'diagnostics' AS latest_sample FROM visible v
             LEFT JOIN cvm_observation_state o ON o.instance_id=v.id ORDER BY v.id"
        ))
        .bind(tenant)
        .bind(pattern)
        .bind(user)
        .fetch_all(&self.pool)
        .await?;
        let ids = rows
            .iter()
            .map(|r| r.try_get("id"))
            .collect::<std::result::Result<Vec<String>, _>>()?;
        // Each lateral scan uses the per-instance history index and at most 60
        // minute samples. Unrelated tenants cannot consume the list bound.
        let history = sqlx::query(
            "SELECT i.id,s.sample FROM unnest($1::text[]) i(id)
             CROSS JOIN LATERAL (SELECT sample-'diagnostics' AS sample FROM cvm_observation_samples
               WHERE instance_id=i.id AND bucket_millis >= $2 AND observed_at >= $3
               ORDER BY bucket_millis DESC,observed_at DESC LIMIT 60) s",
        )
        .bind(&ids)
        .bind((now.saturating_sub(3_600_000) / 60_000 * 60_000) as i64)
        .bind(now.saturating_sub(3_600_000) as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut samples: BTreeMap<String, Vec<CvmCardSample>> = BTreeMap::new();
        for row in history {
            let sample: MonitoringSample = serde_json::from_value(row.try_get("sample")?)?;
            samples
                .entry(row.try_get("id")?)
                .or_default()
                .push(sample.into());
        }
        let usage = sqlx::query(
            "SELECT a.interface_id,u.uploaded,u.downloaded,u.coverage_gap,u.observed_at
             FROM cvm_nic_allocations a JOIN cvm_nic_monthly_usage u ON u.interface_id=a.interface_id
             WHERE a.instance_id=ANY($1) AND NOT a.released
             AND u.period=date_trunc('month',now() AT TIME ZONE 'UTC')::date"
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;
        let mut monthly = BTreeMap::new();
        for row in usage {
            monthly.insert(
                row.try_get::<String, _>("interface_id")?,
                (
                    row.try_get::<i64, _>("uploaded")? as u64,
                    row.try_get::<i64, _>("downloaded")? as u64,
                    row.try_get::<bool, _>("coverage_gap")?,
                    row.try_get::<i64, _>("observed_at")? as u64,
                ),
            );
        }
        let mut instances = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id")?;
            let desired: InstanceDesired = serde_json::from_value(row.try_get("desired")?)?;
            let health: Option<InstanceObservation> = row
                .try_get::<Option<serde_json::Value>, _>("observation")?
                .map(serde_json::from_value)
                .transpose()?;
            let latest: Option<MonitoringSample> = row
                .try_get::<Option<serde_json::Value>, _>("latest_sample")?
                .map(serde_json::from_value)
                .transpose()?;
            let state = match health.as_ref() {
                Some(h) if h.runtime_state == ObservedRuntimeState::Stopped => "stopped",
                Some(h)
                    if h.runtime_state == ObservedRuntimeState::Running
                        && h.error.is_none()
                        && now.saturating_sub(h.observed_at_unix_millis) <= 120_000
                        && latest.as_ref().is_some_and(|s| {
                            now.saturating_sub(s.observed_at_unix_millis) <= 120_000
                        }) =>
                {
                    "live"
                }
                Some(h) if h.error.is_some() => "unavailable",
                _ if latest.is_some() => "stale",
                _ => "unavailable",
            };
            let nics: Vec<_> = desired
                .spec
                .interfaces
                .iter()
                .filter_map(|n| n.allocated.as_ref().map(|a| (&n.id, a)))
                .collect();
            let used = nics
                .iter()
                .try_fold(0_u64, |sum, (id, _)| {
                    let (up, down, _, _) = monthly.get(*id)?;
                    sum.checked_add(*up)?.checked_add(*down)
                })
                .filter(|_| !nics.is_empty());
            // A mixture of capped and unlimited NICs has no aggregate ceiling.
            let limit = nics
                .iter()
                .try_fold(0_u64, |sum, (_, nic)| {
                    sum.checked_add(nic.limits.monthly_bytes?)
                })
                .filter(|_| !nics.is_empty());
            let mut history = samples.remove(&id).unwrap_or_default();
            history.sort_by_key(|s| s.observed_at_unix_millis);
            instances.push(CvmCardReading {
                id,
                state,
                latest: latest.map(Into::into),
                samples: history,
                month_used_bytes: used,
                month_limit_bytes: limit,
                month_coverage_gap: nics
                    .iter()
                    .any(|(id, _)| monthly.get(*id).is_none_or(|m| m.2)),
                month_observed_at_unix_millis: nics
                    .iter()
                    .filter_map(|(id, _)| monthly.get(*id).map(|m| m.3))
                    .min(),
            });
        }
        Ok(CvmReadings {
            observed_at_unix_millis: now,
            instances,
        })
    }
}
