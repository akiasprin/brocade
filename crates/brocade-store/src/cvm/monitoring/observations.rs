use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringIssue {
    pub kind: String,
    pub severity: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringEvent {
    pub at_unix_millis: u64,
    pub kind: String,
    pub detail: String,
}

fn counter_mean(
    before: Option<u64>,
    after: Option<u64>,
    old_weight: u64,
    new_weight: u64,
) -> Option<u64> {
    if old_weight == 0 {
        return after;
    }
    match (before, after) {
        (Some(a), Some(b)) => Some(
            ((u128::from(a) * u128::from(old_weight) + u128::from(b) * u128::from(new_weight))
                / u128::from(old_weight + new_weight)) as u64,
        ),
        _ => None,
    }
}
fn mean(before: Option<f64>, after: Option<f64>, old_weight: u64, new_weight: u64) -> Option<f64> {
    if old_weight == 0 {
        return after;
    }
    match (before, after) {
        (Some(a), Some(b)) => {
            Some((a * old_weight as f64 + b * new_weight as f64) / (old_weight + new_weight) as f64)
        }
        _ => None,
    }
}
fn sum(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    before.zip(after).map(|(a, b)| a.saturating_add(b))
}
fn aggregate(
    old: &MonitoringSample,
    new: &MonitoringSample,
    old_weight: u64,
    new_weight: u64,
) -> MonitoringSample {
    let mut out = new.clone();
    out.window_start_unix_millis = old
        .window_start_unix_millis
        .min(new.window_start_unix_millis);
    out.has_gap |= old.has_gap;
    out.cpu_usage_basis_points = counter_mean(
        old.cpu_usage_basis_points.map(u64::from),
        new.cpu_usage_basis_points.map(u64::from),
        old_weight,
        new_weight,
    )
    .map(|n| n as u32);
    out.cpu_peak_basis_points = old
        .cpu_peak_basis_points
        .into_iter()
        .chain(new.cpu_peak_basis_points)
        .max();
    out.memory_available_min_bytes = old
        .memory_available_min_bytes
        .into_iter()
        .chain(new.memory_available_min_bytes)
        .min();
    if old_weight == 0 {
        return out;
    }
    for n in &mut out.interfaces {
        if let Some(p) = old
            .interfaces
            .iter()
            .find(|p| p.id == n.id && p.device == n.device)
        {
            n.rx_bps = counter_mean(p.rx_bps, n.rx_bps, old_weight, new_weight);
            n.tx_bps = counter_mean(p.tx_bps, n.tx_bps, old_weight, new_weight);
            n.rx_pps = mean(p.rx_pps, n.rx_pps, old_weight, new_weight);
            n.tx_pps = mean(p.tx_pps, n.tx_pps, old_weight, new_weight);
            n.drops = sum(p.drops, n.drops);
            n.errors = sum(p.errors, n.errors);
        }
    }
    if let Some((before, after)) = old.diagnostics.as_ref().zip(out.diagnostics.as_mut()) {
        after
            .cpus
            .retain(|c| before.cpus.iter().any(|b| b.cpu == c.cpu));
        for c in &mut after.cpus {
            if let Some(b) = before.cpus.iter().find(|b| b.cpu == c.cpu) {
                let average = |a, b| mean(Some(a), Some(b), old_weight, new_weight).unwrap_or(b);
                c.user = average(b.user, c.user);
                c.system = average(b.system, c.system);
                c.softirq = average(b.softirq, c.softirq);
                c.iowait = average(b.iowait, c.iowait);
                c.steal = average(b.steal, c.steal);
            }
        }
        after.oom_kills = sum(before.oom_kills, after.oom_kills);
        after.major_faults = sum(before.major_faults, after.major_faults);
        for (key, value) in &mut after.tcp {
            if key != "CurrEstab" {
                if let Some(b) = before.tcp.get(key) {
                    *value = value.saturating_add(*b);
                }
            }
        }
        if let Some((b, a)) = before.disk_io.as_ref().zip(after.disk_io.as_mut()) {
            // Await is weighted by completed operations, rates by elapsed time.
            let op_mean = |old_await: Option<f64>,
                           new_await: Option<f64>,
                           old_iops: Option<f64>,
                           new_iops: Option<f64>| {
                let w0 = old_iops.unwrap_or(0.0) * old_weight as f64;
                let w1 = new_iops.unwrap_or(0.0) * new_weight as f64;
                if (w0 > 0.0 && old_await.is_none()) || (w1 > 0.0 && new_await.is_none()) {
                    return None;
                }
                if w0 + w1 > 0.0 {
                    Some(
                        (old_await.unwrap_or(0.0) * w0 + new_await.unwrap_or(0.0) * w1) / (w0 + w1),
                    )
                } else {
                    None
                }
            };
            a.read_await_ms = op_mean(b.read_await_ms, a.read_await_ms, b.read_iops, a.read_iops);
            a.write_await_ms = op_mean(
                b.write_await_ms,
                a.write_await_ms,
                b.write_iops,
                a.write_iops,
            );
            a.read_bps = counter_mean(b.read_bps, a.read_bps, old_weight, new_weight);
            a.write_bps = counter_mean(b.write_bps, a.write_bps, old_weight, new_weight);
            a.read_iops = mean(b.read_iops, a.read_iops, old_weight, new_weight);
            a.write_iops = mean(b.write_iops, a.write_iops, old_weight, new_weight);
            a.busy_pct = mean(b.busy_pct, a.busy_pct, old_weight, new_weight);
            a.queue_depth = mean(b.queue_depth, a.queue_depth, old_weight, new_weight);
        }
    }
    out
}
pub(super) fn issues(
    latest: Option<&MonitoringSample>,
    health: Option<&InstanceObservation>,
    fresh: bool,
) -> Vec<MonitoringIssue> {
    let mut out = vec![];
    let mut add = |kind: &str, severity: &str, message: String| {
        out.push(MonitoringIssue {
            kind: kind.into(),
            severity: severity.into(),
            message,
        })
    };
    if let Some(h) = health {
        if h.runtime_state == ObservedRuntimeState::Missing {
            add("runtime", "bad", "未发现实例运行时，需要核查".into());
        }
        if h.runtime_state == ObservedRuntimeState::Running
            && h.guest_state == Reachability::Unreachable
        {
            add("guest", "bad", "运行时存在，Guest 采样通道无响应".into());
        }
        if h.ssh_state == Reachability::Unreachable {
            add("ssh", "warn", "承载机器无法连接实例 SSH 端口".into());
        }
        if h.services
            .iter()
            .any(|p| p.state == Reachability::Unreachable)
        {
            add(
                "services",
                "warn",
                "已配置的服务端口有连接失败，来源为承载机器".into(),
            );
        }
    }
    if !fresh {
        return out;
    }
    if let Some(s) = latest {
        if s.cpu_usage_basis_points.is_some_and(|n| n >= 8500) {
            add("cpu", "warn", "当前采样窗口 CPU 使用率超过 85%".into());
        }
        if s.memory_total_bytes > 0
            && s.memory_total_bytes - s.memory_used_bytes < s.memory_total_bytes / 10
        {
            add("memory", "warn", "Guest 可用内存低于 10%".into());
        }
        if s.root_total_bytes > 0
            && s.root_total_bytes - s.root_used_bytes < s.root_total_bytes / 20
        {
            add("disk", "bad", "根文件系统剩余空间低于 5%".into());
        }
        if let Some(d) = &s.diagnostics {
            if d.oom_kills.is_some_and(|n| n > 0) {
                add(
                    "oom",
                    "bad",
                    format!("本次采样窗口发生 {} 次 OOM 终止", d.oom_kills.unwrap_or(0)),
                );
            }
            if d.inode_total
                .zip(d.inode_free)
                .is_some_and(|(total, free)| total > 0 && free < total / 20)
            {
                add("inode", "warn", "根文件系统可用 inode 低于 5%".into());
            }
            for p in &d.pressure {
                if p.some_basis_points >= 100 {
                    add(
                        "pressure",
                        "warn",
                        format!(
                            "{} 存在资源等待：{:.2}%（内核近 10 秒口径）",
                            p.resource,
                            p.some_basis_points as f64 / 100.0
                        ),
                    );
                }
            }
        }
        for n in &s.interfaces {
            if n.carrier == Some(false) {
                add("network", "warn", format!("{} 链路未就绪", n.device));
            } else if n.drops.is_some_and(|v| v > 0) || n.errors.is_some_and(|v| v > 0) {
                add(
                    "network",
                    "warn",
                    format!("{} 在采样窗口出现丢包或错误", n.device),
                );
            }
        }
    }
    out
}
async fn event(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    at: u64,
    kind: &str,
    detail: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO cvm_observation_events(instance_id,observed_at,kind,detail) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(id).bind(at as i64).bind(kind).bind(detail).execute(&mut **tx).await?;
    Ok(())
}
impl PgStore {
    pub async fn prune_cvm_observations(&self) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='1s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        let cutoff = now_millis()?.saturating_sub(HISTORY_MILLIS + 3_600_000) as i64;
        let samples=sqlx::query("DELETE FROM cvm_observation_samples WHERE ctid IN (SELECT ctid FROM cvm_observation_samples WHERE observed_at<$1 ORDER BY observed_at LIMIT 2048)").bind(cutoff).execute(&mut *tx).await?.rows_affected();
        let events=sqlx::query("DELETE FROM cvm_observation_events WHERE ctid IN (SELECT ctid FROM cvm_observation_events WHERE observed_at<$1 ORDER BY observed_at LIMIT 2048)").bind(cutoff).execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        Ok(samples + events)
    }
    pub async fn cvm_observation_targets(&self, node: &str) -> Result<Vec<ObservationTarget>> {
        let rows=sqlx::query("SELECT id,generation,desired->>'power' AS power FROM cvm_instances WHERE node_id=$1 AND reserved ORDER BY id LIMIT 256").bind(node).fetch_all(&self.pool).await?;
        let mut out = vec![];
        for row in rows {
            let id: String = row.try_get("id")?;
            // Probe the container's TCP entry services from its own host through the
            // primary NIC delivery address, the same path an entry delivers to.
            let probes=sqlx::query("SELECT e.id,e.mode,i.report->>'access_address' AS target,(SELECT device->>'id' FROM jsonb_array_elements(i.desired#>'{spec,interfaces}') device WHERE device#>>'{connection,purpose}'='contact' LIMIT 1) AS interface_id FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id WHERE e.instance_id=$1 AND NOT e.released AND e.mode->>'kind'='l4' ORDER BY e.created_at,e.id LIMIT 64").bind(&id).fetch_all(&self.pool).await?;
            let mut services = Vec::new();
            for probe in probes {
                let (Some(target), Some(interface_id)) = (
                    probe.try_get::<Option<String>, _>("target")?,
                    probe.try_get::<Option<String>, _>("interface_id")?,
                ) else {
                    continue;
                };
                let EntryMode::L4 {
                    protocol,
                    target_port,
                    ..
                } = serde_json::from_value(probe.try_get("mode")?)?
                else {
                    continue;
                };
                if !protocol.covers(L4Protocol::Tcp)
                    || services
                        .iter()
                        .any(|s: &ServiceProbeTarget| s.port == target_port)
                {
                    continue;
                }
                services.push(ServiceProbeTarget {
                    id: probe.try_get("id")?,
                    interface_id,
                    host: target,
                    port: target_port,
                });
                if services.len() == 8 {
                    break;
                }
            }
            out.push(ObservationTarget {
                id,
                generation: row.try_get::<i64, _>("generation")? as u64,
                power: serde_json::from_value(serde_json::Value::String(row.try_get("power")?))?,
                services,
            });
        }
        Ok(out)
    }
    pub async fn cvm_record_observations(
        &self,
        node: &str,
        observations: Vec<InstanceObservation>,
    ) -> Result<usize> {
        if observations.len() > 32 {
            return Err(invalid("CVM observation batch exceeds bound"));
        }
        let now = now_millis()?;
        let mut accepted = 0;
        let mut tx = self.pool.begin().await?;
        for mut o in observations {
            if !valid_id(&o.id)
                || o.generation == 0
                || o.observed_at_unix_millis > now + 60_000
                || o.observed_at_unix_millis < now.saturating_sub(HISTORY_MILLIS)
                || o.services.len() > 8
                || o.runtime_id.as_ref().is_some_and(|v| {
                    v.len() > 150
                        || !v
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
                || o.services
                    .iter()
                    .any(|p| !valid_id(&p.id) || p.latency_millis.is_some_and(|v| v > 60_000))
            {
                return Err(invalid("invalid CVM observation identity or health"));
            }
            let generation: Option<i64> = sqlx::query_scalar(
                "SELECT generation FROM cvm_instances WHERE id=$1 AND node_id=$2",
            )
            .bind(&o.id)
            .bind(node)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(generation) = generation else {
                return Err(StoreError::NotFound("CVM observation assignment".into()));
            };
            if generation as u64 != o.generation {
                continue;
            }
            if o.error.as_ref().is_some_and(|s| s.len() > 300) {
                o.error = Some("实例观测失败".into());
            }
            if o.metrics.as_ref().is_some_and(|m| {
                m.validate_observation(now).is_err()
                    || m.observed_at_unix_millis != o.observed_at_unix_millis
            }) {
                o.metrics = None;
                o.error = Some("Guest 监控读数无效，等待重新采样".into());
            }
            sqlx::query("INSERT INTO cvm_observation_state(instance_id,observed_at,observation) VALUES($1,1,'{}') ON CONFLICT DO NOTHING").bind(&o.id).execute(&mut *tx).await?;
            let old =
                sqlx::query("SELECT * FROM cvm_observation_state WHERE instance_id=$1 FOR UPDATE")
                    .bind(&o.id)
                    .fetch_one(&mut *tx)
                    .await?;
            if o.observed_at_unix_millis <= old.try_get::<i64, _>("observed_at")? as u64 {
                continue;
            }
            let old_raw: serde_json::Value = old.try_get("observation")?;
            let old_health = if old_raw.get("id").is_some() {
                Some(serde_json::from_value::<InstanceObservation>(old_raw)?)
            } else {
                None
            };
            let previous: Option<GuestMetrics> = old
                .try_get::<Option<serde_json::Value>, _>("previous_metrics")?
                .map(serde_json::from_value)
                .transpose()?;
            let mut latest = None;
            if let Some(m) = &o.metrics {
                let continuous = old_health.as_ref().is_none_or(|h| {
                    h.guest_state == Reachability::Reachable
                        && h.error.is_none()
                        && h.runtime_id == o.runtime_id
                });
                let point = sample(m, if continuous { previous.as_ref() } else { None });
                let bucket = (m.observed_at_unix_millis / 60_000 * 60_000) as i64;
                let weight = m
                    .observed_at_unix_millis
                    .saturating_sub(point.window_start_unix_millis)
                    .max(1);
                let prior=sqlx::query("SELECT sample,weight_millis FROM cvm_observation_samples WHERE instance_id=$1 AND bucket_millis=$2 AND boot_id=$3 FOR UPDATE").bind(&o.id).bind(bucket).bind(&m.boot_id).fetch_optional(&mut *tx).await?;
                let (merged, total_weight) = if let Some(p) = prior {
                    let old_point: MonitoringSample = serde_json::from_value(p.try_get("sample")?)?;
                    let w = if old_point.window_start_unix_millis
                        == old_point.observed_at_unix_millis
                    {
                        0
                    } else {
                        p.try_get::<i64, _>("weight_millis")? as u64
                    };
                    (aggregate(&old_point, &point, w, weight), w + weight)
                } else {
                    (point.clone(), weight)
                };
                sqlx::query("INSERT INTO cvm_observation_samples(instance_id,bucket_millis,boot_id,observed_at,weight_millis,sample) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(instance_id,bucket_millis,boot_id) DO UPDATE SET observed_at=excluded.observed_at,weight_millis=excluded.weight_millis,sample=excluded.sample").bind(&o.id).bind(bucket).bind(&m.boot_id).bind(m.observed_at_unix_millis as i64).bind(total_weight as i64).bind(serde_json::to_value(merged)?).execute(&mut *tx).await?;
                if previous.as_ref().is_some_and(|p| p.boot_id != m.boot_id) {
                    event(
                        &mut tx,
                        &o.id,
                        o.observed_at_unix_millis,
                        "reboot",
                        "检测到 Guest 启动身份变化",
                    )
                    .await?;
                }
                if point
                    .diagnostics
                    .as_ref()
                    .and_then(|d| d.oom_kills)
                    .is_some_and(|n| n > 0)
                {
                    event(
                        &mut tx,
                        &o.id,
                        o.observed_at_unix_millis,
                        "oom",
                        "内核报告 OOM 终止",
                    )
                    .await?;
                }
                latest = Some(serde_json::to_value(point)?);
            }
            if let Some(old) = &old_health {
                if old.guest_state != o.guest_state
                    && o.guest_state != Reachability::Unknown
                    && (o.guest_state == Reachability::Unreachable
                        || old.guest_state == Reachability::Unreachable)
                {
                    event(
                        &mut tx,
                        &o.id,
                        o.observed_at_unix_millis,
                        if o.guest_state == Reachability::Reachable {
                            "guest_recovered"
                        } else {
                            "guest_unreachable"
                        },
                        if o.guest_state == Reachability::Reachable {
                            "Guest 采样通道恢复"
                        } else {
                            "Guest 采样通道无响应"
                        },
                    )
                    .await?;
                }
                let before = old
                    .services
                    .iter()
                    .any(|p| p.state == Reachability::Unreachable);
                let after = o
                    .services
                    .iter()
                    .any(|p| p.state == Reachability::Unreachable);
                if before != after
                    && !o.services.is_empty()
                    && (after
                        || o.services
                            .iter()
                            .all(|p| p.state == Reachability::Reachable))
                {
                    event(
                        &mut tx,
                        &o.id,
                        o.observed_at_unix_millis,
                        if after {
                            "service_unreachable"
                        } else {
                            "service_recovered"
                        },
                        if after {
                            "承载机器到服务端口连接失败"
                        } else {
                            "承载机器到服务端口连接恢复"
                        },
                    )
                    .await?;
                }
            }
            sqlx::query("UPDATE cvm_observation_state SET observed_at=$2,observation=$3,previous_metrics=coalesce($4,previous_metrics),latest_sample=coalesce($5,latest_sample),received_at=now() WHERE instance_id=$1").bind(&o.id).bind(o.observed_at_unix_millis as i64).bind(serde_json::to_value(&o)?).bind(o.metrics.as_ref().map(serde_json::to_value).transpose()?).bind(latest).execute(&mut *tx).await?;
            accepted += 1;
        }
        tx.commit().await?;
        Ok(accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn minute_rates_do_not_turn_unknown_intervals_into_zero_or_known_averages() {
        assert_eq!(counter_mean(None, Some(40), 15_000, 15_000), None);
        assert_eq!(mean(Some(1.0), None, 15_000, 15_000), None);
        assert_eq!(counter_mean(None, Some(40), 0, 15_000), Some(40));
        assert_eq!(counter_mean(Some(20), Some(60), 15_000, 15_000), Some(40));
    }
}
