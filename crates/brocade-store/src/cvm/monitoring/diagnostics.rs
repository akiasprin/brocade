use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CpuReading {
    pub cpu: i32,
    pub user: f64,
    pub system: f64,
    pub softirq: f64,
    pub iowait: f64,
    pub steal: f64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskReading {
    pub read_bps: Option<u64>,
    pub write_bps: Option<u64>,
    pub read_iops: Option<f64>,
    pub write_iops: Option<f64>,
    pub read_await_ms: Option<f64>,
    pub write_await_ms: Option<f64>,
    pub busy_pct: Option<f64>,
    pub queue_depth: Option<f64>,
    pub in_flight: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosticReading {
    pub cpus: Vec<CpuReading>,
    pub load: Option<[f64; 3]>,
    pub running_processes: Option<u32>,
    pub total_processes: Option<u32>,
    pub memory: BTreeMap<String, u64>,
    pub swap_used_bytes: Option<u64>,
    pub oom_kills: Option<u64>,
    pub major_faults: Option<u64>,
    pub pressure: Vec<PressureReading>,
    pub inode_total: Option<u64>,
    pub inode_free: Option<u64>,
    pub disk_io: Option<DiskReading>,
    pub tcp: BTreeMap<String, u64>,
    pub unsupported: Vec<String>,
}
fn delta(a: u64, b: u64) -> Option<u64> {
    a.checked_sub(b)
}
pub(super) fn diagnostics(
    now: &GuestMetrics,
    old: Option<&GuestMetrics>,
) -> Option<DiagnosticReading> {
    let d = now.diagnostics.as_ref()?;
    let previous = old.and_then(|p| p.diagnostics.as_ref());
    let millis = old.map(|p| now.uptime_millis - p.uptime_millis);
    let seconds = millis.map(|m| m as f64 / 1000.0);
    let mut cpus = vec![];
    if let Some(p) = previous {
        for c in &d.cpus {
            if let Some(before) = p.cpus.iter().find(|b| b.cpu == c.cpu) {
                let changes = c
                    .ticks
                    .iter()
                    .zip(before.ticks.iter())
                    .map(|(&a, &b)| delta(a, b))
                    .collect::<Option<Vec<_>>>();
                if let Some(t) = changes {
                    let total = t.iter().sum::<u64>();
                    if total > 0 {
                        let pct = |n: u64| n as f64 * 100.0 / total as f64;
                        cpus.push(CpuReading {
                            cpu: c.cpu,
                            user: pct(t[0] + t[1]),
                            system: pct(t[2] + t[5]),
                            softirq: pct(t[6]),
                            iowait: pct(t[4]),
                            steal: pct(t[7]),
                        });
                    }
                }
            }
        }
    }
    let vm_delta = |key: &str| {
        d.vm.get(key)
            .zip(previous.and_then(|p| p.vm.get(key)))
            .and_then(|(&a, &b)| delta(a, b))
    };
    let disk = d
        .disk_io
        .as_ref()
        .zip(previous.and_then(|p| p.disk_io.as_ref()))
        .filter(|(a, b)| a.device == b.device)
        .and_then(|(a, b)| {
            let seconds = seconds?;
            let millis = millis? as f64;
            let reads = delta(a.reads, b.reads)?;
            let writes = delta(a.writes, b.writes)?;
            Some(DiskReading {
                read_bps: delta(a.read_bytes, b.read_bytes).map(|n| (n as f64 / seconds) as u64),
                write_bps: delta(a.write_bytes, b.write_bytes).map(|n| (n as f64 / seconds) as u64),
                read_iops: Some(reads as f64 / seconds),
                write_iops: Some(writes as f64 / seconds),
                read_await_ms: delta(a.read_millis, b.read_millis)
                    .and_then(|n| (reads > 0).then_some(n as f64 / reads.max(1) as f64)),
                write_await_ms: delta(a.write_millis, b.write_millis)
                    .and_then(|n| (writes > 0).then_some(n as f64 / writes.max(1) as f64)),
                busy_pct: delta(a.busy_millis, b.busy_millis)
                    .map(|n| (n as f64 * 100.0 / millis).min(100.0)),
                queue_depth: delta(a.weighted_millis, b.weighted_millis).map(|n| n as f64 / millis),
                in_flight: a.in_flight,
            })
        });
    let mut tcp = BTreeMap::new();
    for (key, &value) in &d.tcp {
        if key == "CurrEstab" {
            tcp.insert(key.clone(), value);
        } else if let Some(n) = previous
            .and_then(|p| p.tcp.get(key))
            .and_then(|&b| delta(value, b))
        {
            tcp.insert(key.clone(), n);
        }
    }
    Some(DiagnosticReading {
        cpus,
        load: d.load_milli.map(|a| a.map(|n| n as f64 / 1000.0)),
        running_processes: d.running_processes,
        total_processes: d.total_processes,
        memory: d.memory.clone(),
        swap_used_bytes: d
            .memory
            .get("SwapTotal")
            .zip(d.memory.get("SwapFree"))
            .and_then(|(&a, &b)| delta(a, b)),
        oom_kills: vm_delta("oom_kill"),
        major_faults: vm_delta("pgmajfault"),
        pressure: d.pressure.clone(),
        inode_total: d.inode_total,
        inode_free: d.inode_free,
        disk_io: disk,
        tcp,
        unsupported: d.unsupported.clone(),
    })
}
