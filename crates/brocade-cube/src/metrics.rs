use crate::{backend::Inner, Result};
use brocade_deployment::cvm::*;
use std::{collections::BTreeMap, time::Duration};

const SAFE: u64 = (1u64 << 53) - 1;
const SCRIPT: &str = include_str!("guest_metrics.sh");

impl Inner {
    pub(crate) fn guest_metrics(
        &self,
        runtime: &str,
        interfaces: &[InterfaceObservation],
    ) -> Result<GuestMetrics> {
        let bytes = self.guest_exec(
            runtime,
            false,
            &["/bin/sh", "-c", SCRIPT],
            Duration::from_secs(4),
        )?;
        parse(&bytes, interfaces, crate::now_millis()?)
    }
}

pub(crate) fn parse(
    raw: &[u8],
    interfaces: &[InterfaceObservation],
    at: u64,
) -> Result<GuestMetrics> {
    let bad = "incomplete or invalid Guest monitoring data";
    if raw.len() > 32 * 1024 || interfaces.len() > MAX_INTERFACES {
        return Err(bad.into());
    }
    let text = std::str::from_utf8(raw).map_err(|_| bad)?;
    let mut values = BTreeMap::<&str, Vec<&str>>::new();
    let mut cores = vec![];
    let mut nics = BTreeMap::new();
    for line in text.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 2 {
            continue;
        }
        if fields[0] == "core" {
            if cores.len() < 32 {
                cores.push(fields[1..].to_vec());
            }
            continue;
        }
        if fields[0] == "net" {
            if fields.len() != 5 || nics.insert(fields[1], fields[2..].to_vec()).is_some() {
                return Err(bad.into());
            }
        } else if values.insert(fields[0], fields[1..].to_vec()).is_some() {
            return Err(bad.into());
        }
    }
    let read = |key: &str, count: usize| -> Result<Vec<u64>> {
        let fields = values.get(key).filter(|v| v.len() == count).ok_or(bad)?;
        fields
            .iter()
            .map(|field| {
                field
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n <= SAFE / 1024)
                    .ok_or_else(|| bad.into())
            })
            .collect()
    };
    let cpu = read("cpu", 8)?;
    let memory = read("mem_total", 1)?[0];
    let available = read("mem_available", 1)?[0];
    let root = read("root", 2)?;
    let boot = values
        .get("boot")
        .filter(|v| v.len() == 1 && v[0].len() == 36)
        .ok_or(bad)?[0];
    let uptime = values
        .get("uptime")
        .filter(|v| v.len() == 1)
        .and_then(|v| v[0].parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= SAFE as f64 / 1000.0)
        .ok_or(bad)?;
    if memory == 0 || available > memory || root[0] == 0 || root[1] > root[0] {
        return Err(bad.into());
    }
    let mut counters = vec![];
    for nic in interfaces {
        let fields = nics
            .get(nic.device.as_str())
            .filter(|v| v.len() == 3)
            .ok_or(bad)?;
        let numbers = fields
            .iter()
            .map(|v| v.parse::<u64>().ok().filter(|v| *v <= SAFE).ok_or(bad))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let ifindex = u32::try_from(numbers[0])
            .ok()
            .filter(|n| *n > 0)
            .ok_or(bad)?;
        let extras = values
            .get(format!("net_detail_{}", nic.device).as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let optional = |i: usize| extras.get(i).copied().and_then(metric);
        counters.push(InterfaceCounters {
            id: nic.id.clone(),
            device: nic.device.clone(),
            ifindex,
            rx_bytes: numbers[1],
            tx_bytes: numbers[2],
            rx_packets: optional(0),
            tx_packets: optional(1),
            rx_dropped: optional(2),
            tx_dropped: optional(3),
            rx_errors: optional(4),
            tx_errors: optional(5),
            carrier: if extras.len() == 7 {
                match extras[6] {
                    "0" => Some(false),
                    "1" => Some(true),
                    _ => None,
                }
            } else {
                None
            },
        });
    }
    let aggregate: [u64; 8] = cpu.clone().try_into().map_err(|_| bad)?;
    let diagnostics = diagnostics(&values, &cores, aggregate);
    let result = GuestMetrics {
        diagnostics: Some(diagnostics),
        observed_at_unix_millis: at,
        boot_id: boot.into(),
        uptime_millis: (uptime * 1000.0) as u64,
        cpu_total_ticks: cpu.iter().sum(),
        cpu_busy_ticks: cpu[0] + cpu[1] + cpu[2] + cpu[5] + cpu[6],
        memory_total_bytes: memory * 1024,
        memory_available_bytes: available * 1024,
        root_total_bytes: root[0] * 1024,
        root_used_bytes: root[1] * 1024,
        interfaces: counters,
    };
    result.validate_observation(at).map_err(str::to_owned)?;
    Ok(result)
}

fn metric(raw: &str) -> Option<u64> {
    raw.parse::<u64>().ok().filter(|n| *n <= SAFE)
}
fn scaled(raw: &str, scale: f64) -> Option<u64> {
    raw.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.0 && n * scale <= SAFE as f64)
        .map(|n| (n * scale) as u64)
}

fn diagnostics(
    values: &BTreeMap<&str, Vec<&str>>,
    cores: &[Vec<&str>],
    aggregate: [u64; 8],
) -> GuestDiagnostics {
    let mut out = GuestDiagnostics::default();
    out.cpus.push(CpuCounters {
        cpu: -1,
        ticks: aggregate,
    });
    for fields in cores {
        if fields.len() != 9 {
            continue;
        }
        let Some(id) = fields[0].parse::<i32>().ok().filter(|id| *id >= 0) else {
            continue;
        };
        if let Some(ticks) = fields[1..]
            .iter()
            .map(|v| metric(v))
            .collect::<Option<Vec<_>>>()
            .and_then(|v| v.try_into().ok())
        {
            out.cpus.push(CpuCounters { cpu: id, ticks });
        }
    }
    if let Some(fields) = values.get("load").filter(|v| v.len() == 3) {
        out.load_milli = fields
            .iter()
            .map(|v| scaled(v, 1000.0).and_then(|n| u32::try_from(n).ok()))
            .collect::<Option<Vec<_>>>()
            .and_then(|v| v.try_into().ok());
    }
    if let Some(fields) = values.get("processes").filter(|v| v.len() == 2) {
        out.running_processes = metric(fields[0]).and_then(|n| u32::try_from(n).ok());
        out.total_processes = metric(fields[1]).and_then(|n| u32::try_from(n).ok());
    }
    for (key, fields) in values {
        if fields.len() != 1 {
            continue;
        }
        let Some(n) = metric(fields[0]) else {
            continue;
        };
        if let Some(name) = key.strip_prefix("memory_") {
            if n <= (SAFE / 1024) {
                out.memory.insert(name.into(), n * 1024);
            }
        }
        if let Some(name) = key.strip_prefix("vm_") {
            out.vm.insert(name.into(), n);
        }
        if let Some(name) = key.strip_prefix("tcp_") {
            out.tcp.insert(name.into(), n);
        }
    }
    for kind in ["cpu", "memory", "io"] {
        let some_key = format!("pressure_{kind}_some");
        let Some(fields) = values.get(some_key.as_str()).filter(|v| v.len() == 1) else {
            out.unsupported.push(format!("pressure_{kind}"));
            continue;
        };
        let Some(some) = scaled(fields[0], 100.0).filter(|n| *n <= 10000) else {
            continue;
        };
        let full_key = format!("pressure_{kind}_full");
        let full = if kind == "cpu" {
            None
        } else {
            values
                .get(full_key.as_str())
                .filter(|v| v.len() == 1)
                .and_then(|v| scaled(v[0], 100.0))
                .filter(|n| *n <= 10000)
                .map(|n| n as u32)
        };
        out.pressure.push(PressureReading {
            resource: kind.into(),
            some_basis_points: some as u32,
            full_basis_points: full,
        });
    }
    if let Some(fields) = values.get("inode").filter(|v| v.len() == 2) {
        out.inode_total = metric(fields[0]);
        out.inode_free = metric(fields[1]);
    }
    if let Some(fields) = values.get("diskio").filter(|v| v.len() == 10) {
        if let Some(mut n) = fields[1..]
            .iter()
            .map(|v| metric(v))
            .collect::<Option<Vec<_>>>()
        {
            if n[2] <= SAFE / 512 && n[3] <= SAFE / 512 {
                n[2] *= 512;
                n[3] *= 512;
                out.disk_io = Some(DiskCounters {
                    device: fields[0].into(),
                    reads: n[0],
                    writes: n[1],
                    read_bytes: n[2],
                    write_bytes: n[3],
                    read_millis: n[4],
                    write_millis: n[5],
                    busy_millis: n[6],
                    weighted_millis: n[7],
                    in_flight: n[8],
                });
            }
        }
    }
    if out.disk_io.is_none() {
        out.unsupported.push("root_block_io".into());
    }
    out
}
