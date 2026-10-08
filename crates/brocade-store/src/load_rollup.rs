//! Minute history aggregation. Keep actual windows, event totals, unknowns and reboot boundaries.
use std::collections::BTreeMap;

use brocade_deployment::protocol::{CpuCoreSample, LoadSample};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{Result, StoreError};

pub(crate) const RAW_RETAIN_SECS: i64 = 3600;
const MINUTE: i64 = 60;
const BATCH_SAMPLES: i64 = 240;
const BATCH_NODES: i64 = 16;
// "LOAD": retention must not delete sources between our read and replacement inserts.
pub(crate) const RETENTION_LOCK: i64 = 0x4c4f4144;

pub(crate) fn sample_from_row(row: &sqlx::postgres::PgRow) -> Result<TimedSample> {
    Ok(TimedSample {
        btime: row.try_get("btime")?,
        sample: crate::load::load_sample_from_row(row)?,
    })
}

/// Shared with ingestion: retries behind the fence cannot recreate already-compacted inputs.
/// Lock the parent first to keep ON DELETE CASCADE in the same lock order.
pub(crate) async fn lock_node(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<i64> {
    sqlx::query("SELECT id FROM nodes WHERE id = $1 FOR KEY SHARE")
        .bind(node)
        .fetch_optional(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO node_load_compaction_state (node_id) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(node)
    .execute(&mut **tx)
    .await?;
    Ok(sqlx::query_scalar("SELECT extract(epoch FROM sealed_through)::bigint FROM node_load_compaction_state WHERE node_id = $1 FOR UPDATE")
        .bind(node).fetch_one(&mut **tx).await?)
}

/// A bounded maintenance pass. Return the number of raw rows sealed, including one-row segments.
/// Rewriting and fencing are atomic; history readers see either the sources or their aggregate.
pub(crate) async fn compact(pool: &PgPool) -> Result<u64> {
    let cutoff: i64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM date_trunc('minute', now() - interval '1 hour'))::bigint",
    )
    .fetch_one(pool)
    .await?;
    let nodes: Vec<String> = sqlx::query_scalar(
        "SELECT n.id FROM nodes n LEFT JOIN node_load_compaction_state s ON s.node_id = n.id
         WHERE s.sealed_through IS NULL OR s.sealed_through < to_timestamp($1)
         ORDER BY s.sealed_through NULLS FIRST, n.id LIMIT $2",
    )
    .bind(cutoff)
    .bind(BATCH_NODES)
    .fetch_all(pool)
    .await?;
    let mut processed = 0;
    for node in nodes {
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout = '3s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL lock_timeout = '1s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
            .bind(RETENTION_LOCK)
            .execute(&mut *tx)
            .await?;
        let sealed = lock_node(&mut tx, &node).await?;
        if sealed >= cutoff {
            tx.commit().await?;
            continue;
        }
        // The latest raw window remains an exact snapshot even when an offline node has no
        // recent data. The normal seven-day retention still applies to it.
        let rows = sqlx::query(
            "SELECT *, extract(epoch FROM window_start)::bigint AS window_start_secs,
                       extract(epoch FROM window_end)::bigint AS window_end_secs
             FROM node_load_samples WHERE node_id = $1 AND NOT is_rollup
               AND window_end <= to_timestamp($2)
               AND window_start < (SELECT max(window_start) FROM node_load_samples WHERE node_id = $1)
             ORDER BY window_end, window_start LIMIT $3")
            .bind(&node).bind(cutoff).bind(BATCH_SAMPLES + 1).fetch_all(&mut *tx).await?;
        let mut samples = rows
            .iter()
            .map(sample_from_row)
            .collect::<Result<Vec<_>>>()?;
        let complete = samples.len() <= BATCH_SAMPLES as usize;
        if !complete {
            let last_bucket = bucket_end(&samples.last().expect("full batch").sample);
            // Do not split a minute across commits. A pathological batch of overlapping long
            // windows is rejected instead of partially sealing it and dropping the remainder.
            while samples
                .last()
                .is_some_and(|s| bucket_end(&s.sample) >= last_bucket)
            {
                samples.pop();
            }
            if samples.is_empty() {
                return Err(StoreError::InvalidData(
                    "load rollup batch contains too many overlapping windows".to_owned(),
                ));
            }
        }
        let watermark = if complete {
            cutoff
        } else {
            bucket_end(&samples.last().expect("nonempty batch").sample)
        };
        let starts = samples
            .iter()
            .map(|s| s.sample.window_start_unix_secs)
            .collect::<Vec<_>>();
        samples.sort_by_key(|s| s.sample.window_start_unix_secs);
        let rolled = aggregate(samples)?;
        sqlx::query("DELETE FROM node_load_samples WHERE node_id = $1 AND window_start IN (SELECT to_timestamp(value) FROM unnest($2::bigint[]) AS value)")
            .bind(&node).bind(&starts).execute(&mut *tx).await?;
        for sample in rolled {
            if !crate::load::insert_load_sample(&mut tx, &node, sample.btime, &sample.sample, true)
                .await?
            {
                return Err(StoreError::InvalidData(
                    "load rollup unexpectedly conflicts with another window".to_owned(),
                ));
            }
        }
        sqlx::query("UPDATE node_load_compaction_state SET sealed_through = greatest(sealed_through, to_timestamp($2)) WHERE node_id = $1")
            .bind(&node).bind(watermark).execute(&mut *tx).await?;
        tx.commit().await?;
        processed += starts.len() as u64;
    }
    Ok(processed)
}

#[derive(Clone)]
pub(crate) struct TimedSample {
    pub btime: i64,
    pub sample: LoadSample,
}

pub(crate) fn bucket_end(sample: &LoadSample) -> i64 {
    (sample.window_end_unix_secs - 1).div_euclid(MINUTE) * MINUTE + MINUTE
}

/// Inputs must be oldest first. A minute can legitimately contain several segments when a
/// machine reboots, a probe is incomplete, or windows do not touch. Never bridge such a boundary.
pub(crate) fn aggregate(samples: Vec<TimedSample>) -> Result<Vec<TimedSample>> {
    let mut result = Vec::new();
    let mut group = Vec::<TimedSample>::new();
    for sample in samples {
        let continues = group.last().is_some_and(|previous| {
            previous.btime == sample.btime
                && bucket_end(&previous.sample) == bucket_end(&sample.sample)
                && !previous.sample.has_gap
                && !sample.sample.has_gap
                && previous.sample.window_end_unix_secs == sample.sample.window_start_unix_secs
                && sample.sample.window_end_unix_secs - group[0].sample.window_start_unix_secs
                    <= MINUTE
        });
        if !continues && !group.is_empty() {
            result.push(merge(&group)?);
            group.clear();
        }
        group.push(sample);
    }
    if !group.is_empty() {
        result.push(merge(&group)?);
    }
    Ok(result)
}

fn sum(mut values: impl Iterator<Item = u64>) -> Result<u64> {
    values.try_fold(0u64, |total, value| {
        total
            .checked_add(value)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or_else(|| {
                StoreError::InvalidData("load rollup counter exceeds storage range".to_owned())
            })
    })
}

fn mean(samples: &[TimedSample], read: impl Fn(&LoadSample) -> f64) -> f64 {
    let duration: i64 = samples
        .iter()
        .map(|s| s.sample.window_end_unix_secs - s.sample.window_start_unix_secs)
        .sum();
    samples
        .iter()
        .map(|s| {
            read(&s.sample)
                * (s.sample.window_end_unix_secs - s.sample.window_start_unix_secs) as f64
        })
        .sum::<f64>()
        / duration as f64
}

fn optional_mean(
    samples: &[TimedSample],
    read: impl Fn(&LoadSample) -> Option<f64>,
) -> Option<f64> {
    samples
        .iter()
        .all(|s| read(&s.sample).is_some())
        .then(|| mean(samples, |s| read(s).expect("all values present")))
}

fn optional_sum(
    samples: &[TimedSample],
    read: impl Fn(&LoadSample) -> Option<u64>,
) -> Result<Option<u64>> {
    if !samples.iter().all(|s| read(&s.sample).is_some()) {
        return Ok(None);
    }
    sum(samples
        .iter()
        .map(|s| read(&s.sample).expect("all values present")))
    .map(Some)
}

fn await_mean(samples: &[TimedSample], read: bool) -> Option<f32> {
    let mut weighted = 0.0;
    let mut operations = 0.0;
    for timed in samples {
        let sample = &timed.sample;
        let disk = sample.disk_detail.as_ref()?;
        let (iops, latency) = if read {
            (disk.read_iops?, disk.read_await_ms)
        } else {
            (disk.write_iops?, disk.write_await_ms)
        };
        let weight =
            f64::from(iops) * (sample.window_end_unix_secs - sample.window_start_unix_secs) as f64;
        if weight > 0.0 {
            weighted += f64::from(latency?) * weight;
            operations += weight;
        }
    }
    (operations > 0.0).then(|| (weighted / operations) as f32)
}

fn merge(samples: &[TimedSample]) -> Result<TimedSample> {
    let mut result = samples.last().expect("nonempty aggregation group").clone();
    if samples.len() == 1 {
        return Ok(result);
    }
    let out = &mut result.sample;
    out.window_start_unix_secs = samples[0].sample.window_start_unix_secs;
    // Snapshot fields remain a coherent copy of the last row. Only interval measurements change.
    macro_rules! average { ($($field:ident),* $(,)?) => {$ (
        out.$field = mean(samples, |s| s.$field as f64) as _;
    )*}; }
    macro_rules! total { ($($field:ident),* $(,)?) => {$ (
        out.$field = sum(samples.iter().map(|s| s.sample.$field))?;
    )*}; }
    average!(
        cpu_user_pct,
        cpu_sys_pct,
        cpu_softirq_pct,
        cpu_steal_pct,
        load1,
        nic_rx_bps,
        nic_tx_bps
    );
    total!(oom_kills, nic_rx_drop, nic_tx_drop, nic_err);
    out.cpu_peak_pct = samples
        .iter()
        .map(|s| s.sample.cpu_peak_pct)
        .fold(0.0, f32::max);

    macro_rules! detail_mean { ($detail:ident; $($field:ident),* $(,)?) => {$ (
        if let Some(detail) = out.$detail.as_mut() {
            detail.$field = mean(samples, |s| s.$detail.as_ref().expect("complete detail group").$field as f64) as _;
        }
    )*}; }
    macro_rules! detail_optional_mean { ($detail:ident; $($field:ident),* $(,)?) => {$ (
        if let Some(detail) = out.$detail.as_mut() {
            detail.$field = optional_mean(samples, |s| s.$detail.as_ref()?.$field.map(|v| v as f64)).map(|v| v as _);
        }
    )*}; }
    macro_rules! detail_sum { ($detail:ident; $($field:ident),* $(,)?) => {$ (
        if let Some(detail) = out.$detail.as_mut() {
            detail.$field = sum(samples.iter().map(|s| s.sample.$detail.as_ref().expect("complete detail group").$field))?;
        }
    )*}; }
    macro_rules! detail_optional_sum { ($detail:ident; $($field:ident),* $(,)?) => {$ (
        if let Some(detail) = out.$detail.as_mut() {
            detail.$field = optional_sum(samples, |s| s.$detail.as_ref()?.$field)?;
        }
    )*}; }

    // Partial optional diagnostics remain unknown, rather than spreading a single known reading
    // over a full minute. Current capabilities are served separately in latest_sample.
    if samples.iter().any(|s| s.sample.cpu_detail.is_none()) {
        out.cpu_detail = None;
    }
    if samples.iter().any(|s| s.sample.memory_detail.is_none()) {
        out.memory_detail = None;
    }
    if samples.iter().any(|s| s.sample.disk_detail.is_none()) {
        out.disk_detail = None;
    }
    if samples.iter().any(|s| s.sample.network_detail.is_none()) {
        out.network_detail = None;
    }
    detail_mean!(cpu_detail; iowait_pct,load5,load15);
    detail_optional_mean!(cpu_detail; pressure_some_pct,io_pressure_some_pct,io_pressure_full_pct,
        context_switches_per_sec,net_rx_softirqs_per_sec,net_tx_softirqs_per_sec,frequency_mhz);
    detail_optional_sum!(cpu_detail; throttled_usec);
    if let Some(detail) = out.cpu_detail.as_mut() {
        let mut cores = BTreeMap::<u32, Vec<(&CpuCoreSample, i64)>>::new();
        for sample in samples {
            for core in &sample
                .sample
                .cpu_detail
                .as_ref()
                .expect("complete detail group")
                .cores
            {
                cores.entry(core.cpu).or_default().push((
                    core,
                    sample.sample.window_end_unix_secs - sample.sample.window_start_unix_secs,
                ));
            }
        }
        detail.cores = cores
            .into_iter()
            .filter(|(_, readings)| readings.len() == samples.len())
            .map(|(cpu, readings)| {
                let total = readings.iter().map(|(_, duration)| *duration).sum::<i64>() as f64;
                let average = |read: fn(&CpuCoreSample) -> f32| {
                    (readings
                        .iter()
                        .map(|(core, duration)| f64::from(read(core)) * *duration as f64)
                        .sum::<f64>()
                        / total) as f32
                };
                CpuCoreSample {
                    cpu,
                    user_pct: average(|c| c.user_pct),
                    system_pct: average(|c| c.system_pct),
                    softirq_pct: average(|c| c.softirq_pct),
                    iowait_pct: average(|c| c.iowait_pct),
                    steal_pct: average(|c| c.steal_pct),
                }
            })
            .collect();
    }
    detail_sum!(memory_detail; swap_in_bytes,swap_out_bytes,major_faults,direct_reclaim_pages);
    detail_optional_mean!(memory_detail; pressure_some_pct,pressure_full_pct);
    if let Some(detail) = out.memory_detail.as_mut() {
        detail.available_min_bytes = samples
            .iter()
            .map(|s| {
                s.sample
                    .memory_detail
                    .as_ref()
                    .expect("complete detail group")
                    .available_min_bytes
            })
            .min()
            .expect("nonempty aggregation group");
    }
    detail_optional_mean!(disk_detail; read_bps,write_bps,read_iops,write_iops,busy_pct,queue_depth,pressure_some_pct,pressure_full_pct);
    if let Some(detail) = out.disk_detail.as_mut() {
        detail.read_await_ms = await_mean(samples, true);
        detail.write_await_ms = await_mean(samples, false);
    }
    detail_optional_sum!(network_detail; tcp_active_opens,tcp_passive_opens,tcp_attempt_fails,tcp_estab_resets,
        tcp_retrans_segs,tcp_syn_retrans,tcp_in_errors,tcp_out_resets,tcp_timeouts,tcp_listen_overflows,tcp_listen_drops,
        udp_in_errors,udp_no_ports,udp_rcvbuf_errors,udp_sndbuf_errors);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use brocade_deployment::protocol::{DiskDetailSample, NetworkDetailSample};
    use serde_json::json;

    fn sample(start: i64, end: i64) -> TimedSample {
        let memory = json!({"available_min_bytes": 80, "free_bytes": 10, "anon_bytes": 20,
            "file_cache_bytes": 30, "shmem_bytes": 5, "kernel_other_bytes": 35,
            "buffers_bytes": 2, "kernel_reclaimable_bytes": 3, "slab_unreclaimable_bytes": 4,
            "unevictable_bytes": 0, "mlocked_bytes": 0, "dirty_bytes": 0, "writeback_bytes": 0,
            "swap_total_bytes": 0, "swap_cached_bytes": 0, "swap_in_bytes": 1,
            "swap_out_bytes": 2, "major_faults": 3, "direct_reclaim_pages": 4});
        TimedSample { btime: 1, sample: serde_json::from_value(json!({
            "window_start_unix_secs": start, "window_end_unix_secs": end, "has_gap": false,
            "cpu_user_pct": 10, "cpu_sys_pct": 5, "cpu_softirq_pct": 1, "cpu_peak_pct": 20,
            "cpu_steal_pct": 0, "load1": 1, "mem_available_bytes": 100, "swap_used_bytes": 0,
            "oom_kills": 1, "disk_free_bytes": 200, "disk_inode_free_pct": 90,
            "nic_rx_bps": 30, "nic_tx_bps": 60, "nic_rx_drop": 2, "nic_tx_drop": 3,
            "nic_err": 4, "conntrack_count": 8, "uptime_secs": end,
            "cpu_detail": {"iowait_pct": 1, "load5": 2, "load15": 3, "throttled_usec": 5,
                "cores": [{"cpu": 0, "user_pct": 10, "system_pct": 5, "softirq_pct": 1, "iowait_pct": 0, "steal_pct": 0}]},
            "memory_detail": memory
        })).unwrap() }
    }

    #[test]
    fn minute_preserves_weighted_rates_peaks_events_and_coherent_snapshot() {
        let first = sample(0, 20);
        let mut last = sample(20, 60);
        last.sample.cpu_user_pct = 40.0;
        last.sample.cpu_peak_pct = 95.0;
        last.sample.nic_rx_bps = 90;
        last.sample.mem_available_bytes = 50;
        last.sample.disk_free_bytes = 150;
        last.sample
            .memory_detail
            .as_mut()
            .unwrap()
            .available_min_bytes = 40;
        last.sample.memory_detail.as_mut().unwrap().free_bytes = 7;
        last.sample.cpu_detail.as_mut().unwrap().cores[0].user_pct = 40.0;
        let mut result = aggregate(vec![first, last]).unwrap();
        assert_eq!(result.len(), 1);
        let s = &result[0].sample;
        assert_eq!((s.window_start_unix_secs, s.window_end_unix_secs), (0, 60));
        assert_eq!(s.cpu_user_pct, 30.0);
        assert_eq!(s.cpu_peak_pct, 95.0);
        assert_eq!(s.nic_rx_bps, 70);
        assert_eq!(
            (s.oom_kills, s.nic_rx_drop, s.nic_tx_drop, s.nic_err),
            (2, 4, 6, 8)
        );
        assert_eq!((s.mem_available_bytes, s.disk_free_bytes), (50, 150));
        assert_eq!(s.memory_detail.as_ref().unwrap().free_bytes, 7);
        assert_eq!(s.memory_detail.as_ref().unwrap().available_min_bytes, 40);
        assert_eq!(s.memory_detail.as_ref().unwrap().major_faults, 6);
        assert_eq!(s.cpu_detail.as_ref().unwrap().throttled_usec, Some(10));
        assert_eq!(s.cpu_detail.as_ref().unwrap().cores[0].user_pct, 30.0);
        let expected = result[0].sample.clone();
        assert_eq!(
            aggregate(std::mem::take(&mut result)).unwrap()[0].sample,
            expected
        );
    }

    #[test]
    fn minute_keeps_missing_optional_values_unknown_and_intersects_cores() {
        let mut first = sample(0, 30);
        let mut last = sample(30, 60);
        first.sample.memory_detail = None;
        first.sample.cpu_detail.as_mut().unwrap().cores.clear();
        last.sample.cpu_detail.as_mut().unwrap().frequency_mhz = Some(2000);
        first.sample.network_detail = Some(NetworkDetailSample {
            tcp_retrans_segs: Some(2),
            ..Default::default()
        });
        last.sample.network_detail = Some(NetworkDetailSample {
            tcp_retrans_segs: Some(3),
            tcp_timeouts: Some(7),
            ..Default::default()
        });
        let s = &aggregate(vec![first, last]).unwrap()[0].sample;
        assert!(s.memory_detail.is_none());
        assert!(s.cpu_detail.as_ref().unwrap().cores.is_empty());
        assert_eq!(s.cpu_detail.as_ref().unwrap().frequency_mhz, None);
        assert_eq!(s.network_detail.as_ref().unwrap().tcp_retrans_segs, Some(5));
        assert_eq!(s.network_detail.as_ref().unwrap().tcp_timeouts, None);
    }

    #[test]
    fn minute_weights_latency_by_completed_io_and_allows_idle_windows() {
        let mut first = sample(0, 20);
        let mut last = sample(20, 60);
        first.sample.disk_detail = Some(DiskDetailSample {
            read_iops: Some(4.0),
            read_await_ms: Some(10.0),
            write_iops: Some(0.0),
            ..Default::default()
        });
        last.sample.disk_detail = Some(DiskDetailSample {
            read_iops: Some(1.0),
            read_await_ms: Some(40.0),
            write_iops: Some(2.0),
            write_await_ms: Some(7.0),
            ..Default::default()
        });
        let s = aggregate(vec![first.clone(), last.clone()])
            .unwrap()
            .remove(0)
            .sample;
        assert_eq!(s.disk_detail.as_ref().unwrap().read_await_ms, Some(20.0));
        assert_eq!(s.disk_detail.as_ref().unwrap().write_await_ms, Some(7.0));
        first.sample.disk_detail.as_mut().unwrap().read_await_ms = None;
        assert_eq!(
            aggregate(vec![first, last]).unwrap()[0]
                .sample
                .disk_detail
                .as_ref()
                .unwrap()
                .read_await_ms,
            None
        );
    }

    #[test]
    fn minute_does_not_bridge_gaps_reboots_overlap_or_bucket_boundaries() {
        for boundary in 0..5 {
            let first = sample(0, 30);
            let mut last = sample(30, 60);
            match boundary {
                0 => last.sample.has_gap = true,
                1 => last.btime = 2,
                2 => last.sample.window_start_unix_secs += 1,
                3 => last.sample.window_start_unix_secs -= 1,
                _ => last.sample.window_end_unix_secs += 1,
            }
            assert_eq!(aggregate(vec![first, last]).unwrap().len(), 2);
        }
        assert_eq!(bucket_end(&sample(30, 60).sample), 60);
        assert_eq!(bucket_end(&sample(31, 61).sample), 120);
    }

    #[test]
    fn minute_counter_overflow_is_an_error_not_a_saturated_or_wrapped_value() {
        let mut first = sample(0, 30);
        first.sample.oom_kills = i64::MAX as u64;
        assert!(aggregate(vec![first, sample(30, 60)]).is_err());
    }
}
