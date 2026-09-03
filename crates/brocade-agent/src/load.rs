//! Host resource sampling, read straight from `/proc` and `statvfs`.
//!
//! No `sysinfo` crate. It reads all of `/proc` to answer any question, while this needs eight
//! small fixed set of kernel files, and it would take the agent's dependency tree from five crates
//! to dozens — which then
//! has to cross-compile to musl and aarch64 along with everything else. The same reasoning as
//! `icmp.rs` writing its own ICMP rather than shelling out to `ping`: the thing being avoided is
//! not the work, it is the dependency on somebody else's idea of what we need.
//!
//! **The agent differences locally and reports rates.** That is the opposite of usage, which ships
//! cumulative counters for the control plane to difference, and the reason for the difference is
//! that usage is money: it has to be auditable and replay-proof, and a previous reading has to be
//! kept somewhere trustworthy. A CPU reading is worth nothing once stale, so keeping one per
//! machine on the control plane would be paid for nothing.
//!
//! What that costs is the control plane's ability to notice a counter reset on its own, which is
//! why every report carries `btime`: it changes exactly when the machine rebooted and every
//! cumulative counter in here went back to zero.

use std::{
    collections::BTreeMap, ffi::CString, fs, os::unix::fs::MetadataExt, path::Path, sync::Mutex,
};

use brocade_deployment::protocol::{
    CpuCoreSample, CpuDetailSample, DiskDetailSample, HostFacts, LoadSample, MemoryDetailSample,
    NetworkDetailSample, ProcessSample,
};

use crate::run_command;

/// Sub-sampling interval. The report window is a multiple of this (see `SUBS_PER_WINDOW`).
///
/// Two rates rather than one: CPU has to be differenced over something short or a spike averages
/// away, while sending every 10 seconds would triple the row count for a resolution nobody reads.
/// So sample at 10s, aggregate to 30s, and carry the peak so the spike survives the aggregation.
pub(crate) const SUB_INTERVAL_SECS: u64 = 10;
/// 10s × 3 = a 30-second window, deliberately the same as usage's. Aligned windows let the console
/// stack a traffic bar and a CPU line on one time axis, which is what makes "the spike follows the
/// traffic" a glance instead of a guess.
pub(crate) const SUBS_PER_WINDOW: usize = 3;

/// The previous sub-sample's raw counters. Differencing needs two readings and the first round has
/// none — it produces nothing rather than reporting everything as zero.
static PREVIOUS: Mutex<Option<Raw>> = Mutex::new(None);
/// Sub-samples accumulated towards the current window.
static PENDING: Mutex<Vec<Sub>> = Mutex::new(Vec::new());

/// One raw reading of the cumulative counters, before differencing.
#[derive(Clone, Default)]
struct Raw {
    at: u64,
    btime: i64,
    /// Total jiffies per class, from `/proc/stat`'s first line.
    cpu: CpuTimes,
    cpu_cores: BTreeMap<u32, CpuTimes>,
    context_switches: Option<u64>,
    cpu_pressure: PressureTotals,
    softirqs: Option<SoftirqCounters>,
    cpu_throttled_usec: Option<u64>,
    /// Sum across the main interface only, not every interface: lo would double every byte, and a
    /// wg0 or tun would count the same traffic again one layer up.
    net: NetCounters,
    /// Host-wide TCP/UDP MIB counters and socket inventory. The former are cumulative here and
    /// differenced together with the other raw counters; the latter are point-in-time levels.
    network: NetworkRaw,
    oom_kills: u64,
    memory: MemInfo,
    vm: VmCounters,
    memory_pressure: PressureTotals,
    io_pressure: PressureTotals,
    /// Counters for the block device backing the agent state directory. `None` on overlay and
    /// network filesystems, where there is no honest device row to attribute.
    disk: Option<DiskCounters>,
    /// utime+stime jiffies per process, keyed by the name we asked for.
    proc_cpu: BTreeMap<String, u64>,
    /// The pids behind `proc_cpu`, carried so the process table does not have to walk /proc again.
    pids: BTreeMap<String, u32>,
}

/// One differenced sub-sample: rates over `SUB_INTERVAL_SECS`.
#[derive(Clone, Default)]
struct Sub {
    window_start_unix_secs: u64,
    window_end_unix_secs: u64,
    elapsed_secs: u64,
    cpu_user_pct: f32,
    cpu_sys_pct: f32,
    cpu_softirq_pct: f32,
    cpu_iowait_pct: f32,
    cpu_steal_pct: f32,
    cpu_cores: Vec<CpuCoreSample>,
    cpu_pressure_some_pct: Option<f32>,
    io_pressure_some_pct: Option<f32>,
    io_pressure_full_pct: Option<f32>,
    context_switches_per_sec: Option<u64>,
    net_rx_softirqs_per_sec: Option<u64>,
    net_tx_softirqs_per_sec: Option<u64>,
    cpu_throttled_usec: Option<u64>,
    nic_rx_bps: u64,
    nic_tx_bps: u64,
    nic_rx_drop: u64,
    nic_tx_drop: u64,
    nic_err: u64,
    network_detail: Option<NetworkDetailSample>,
    oom_kills: u64,
    memory: MemInfo,
    memory_pressure_some_pct: Option<f32>,
    memory_pressure_full_pct: Option<f32>,
    swap_in_bytes: u64,
    swap_out_bytes: u64,
    major_faults: u64,
    direct_reclaim_pages: u64,
    gup_pinned_bytes: Option<u64>,
    disk: Option<DiskDelta>,
    /// Per-process CPU share over this sub-interval.
    proc_cpu_pct: BTreeMap<String, f32>,
    /// btime changed, or the counters went backwards: this sub-sample's differences are not real.
    gap: bool,
}

#[derive(Clone, Copy, Default)]
struct CpuTimes {
    user: u64,
    system: u64,
    softirq: u64,
    /// Waiting for block I/O is not work and therefore stays outside the three busy classes.
    iowait: u64,
    /// Tracked separately from the three reported classes: steal is not work this machine does,
    /// it is the hypervisor serving another guest. See `LoadSample::cpu_steal_pct`.
    steal: u64,
    total: u64,
}

#[derive(Clone, Copy, Default)]
struct PressureTotals {
    some_usec: Option<u64>,
    full_usec: Option<u64>,
}

#[derive(Clone, Copy, Default)]
struct SoftirqCounters {
    net_rx: u64,
    net_tx: u64,
}

#[derive(Clone, Copy, Default)]
struct VmCounters {
    oom_kills: u64,
    swap_in_pages: u64,
    swap_out_pages: u64,
    major_faults: u64,
    direct_reclaim_pages: u64,
    foll_pin_acquired: Option<u64>,
    foll_pin_released: Option<u64>,
}

#[derive(Clone, Copy, Default)]
struct NetCounters {
    rx_bytes: u64,
    tx_bytes: u64,
    rx_drop: u64,
    tx_drop: u64,
    errs: u64,
}

#[derive(Clone, Copy, Default)]
struct DiskCounters {
    major: u64,
    minor: u64,
    reads: u64,
    sectors_read: u64,
    read_ms: u64,
    writes: u64,
    sectors_written: u64,
    write_ms: u64,
    in_flight: u64,
    busy_ms: u64,
    weighted_ms: u64,
}

#[derive(Clone, Copy, Default)]
struct DiskDelta {
    read_bytes: u64,
    write_bytes: u64,
    reads: u64,
    writes: u64,
    read_ms: u64,
    write_ms: u64,
    in_flight: u64,
    busy_ms: u64,
    weighted_ms: u64,
}

/// Raw host-wide network counters. Every field is optional independently: minimal/container
/// kernels sometimes expose sockstat but not TcpExt (or vice versa), and losing the entire network
/// observation because one kernel counter is absent would be worse than preserving that absence.
#[derive(Clone, Copy, Default)]
struct NetworkRaw {
    // End-of-sub-window levels.
    tcp_curr_estab: Option<u64>,
    tcp_inuse: Option<u64>,
    tcp_time_wait: Option<u64>,
    tcp_orphan: Option<u64>,
    tcp_alloc: Option<u64>,
    tcp_mem_pages: Option<u64>,
    udp_inuse: Option<u64>,
    udp_mem_pages: Option<u64>,
    // Cumulative counters, differenced in `difference`.
    tcp_active_opens: Option<u64>,
    tcp_passive_opens: Option<u64>,
    tcp_attempt_fails: Option<u64>,
    tcp_estab_resets: Option<u64>,
    tcp_retrans_segs: Option<u64>,
    tcp_syn_retrans: Option<u64>,
    tcp_in_errors: Option<u64>,
    tcp_out_resets: Option<u64>,
    tcp_timeouts: Option<u64>,
    tcp_listen_overflows: Option<u64>,
    tcp_listen_drops: Option<u64>,
    udp_in_errors: Option<u64>,
    udp_no_ports: Option<u64>,
    udp_rcvbuf_errors: Option<u64>,
    udp_sndbuf_errors: Option<u64>,
}

impl NetworkRaw {
    fn has_any(self) -> bool {
        [
            self.tcp_curr_estab,
            self.tcp_inuse,
            self.tcp_time_wait,
            self.tcp_orphan,
            self.tcp_alloc,
            self.tcp_mem_pages,
            self.udp_inuse,
            self.udp_mem_pages,
            self.tcp_active_opens,
            self.tcp_passive_opens,
            self.tcp_attempt_fails,
            self.tcp_estab_resets,
            self.tcp_retrans_segs,
            self.tcp_syn_retrans,
            self.tcp_in_errors,
            self.tcp_out_resets,
            self.tcp_timeouts,
            self.tcp_listen_overflows,
            self.tcp_listen_drops,
            self.udp_in_errors,
            self.udp_no_ports,
            self.udp_rcvbuf_errors,
            self.udp_sndbuf_errors,
        ]
        .iter()
        .any(Option::is_some)
    }
}

/// Take one sub-sample. Called every `SUB_INTERVAL_SECS`; returns a finished window every
/// `SUBS_PER_WINDOW` calls and `None` in between.
pub(crate) fn tick(state_dir: &Path, now: u64) -> Option<(LoadSample, Vec<ProcessSample>)> {
    let raw = read_raw(now, state_dir);
    let mut previous = PREVIOUS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let sub = previous.as_ref().map(|p| difference(p, &raw));
    *previous = Some(raw.clone());
    drop(previous);

    let sub = sub?;

    let mut pending = PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pending.push(sub);
    if pending.len() < SUBS_PER_WINDOW {
        return None;
    }
    let subs = std::mem::take(&mut *pending);
    drop(pending);

    Some((aggregate(&subs, state_dir), processes(&subs, &raw)))
}

/// The btime of the reading the caller is about to send. Read separately because the report
/// carries it once rather than per sample.
pub(crate) fn current_btime() -> i64 {
    read_btime().unwrap_or(0)
}

fn difference(before: &Raw, now: &Raw) -> Sub {
    let clock_regressed = now.at <= before.at;
    let elapsed = now.at.saturating_sub(before.at).max(1);
    // A reboot resets every counter in here. Treating that as growth would report a machine that
    // just came up as having transferred its whole lifetime's bytes in ten seconds.
    let rebooted = before.btime != now.btime;
    let aggregate_cpu_regressed = cpu_times_regressed(before.cpu, now.cpu);
    let core_cpu_regressed = now.cpu_cores.iter().any(|(cpu, current)| {
        before
            .cpu_cores
            .get(cpu)
            .is_some_and(|previous| cpu_times_regressed(*previous, *current))
    });
    let delayed = elapsed > SUB_INTERVAL_SECS + SUB_INTERVAL_SECS / 2;
    let invalid_interval = rebooted || clock_regressed;
    let cpu_total = now.cpu.total.saturating_sub(before.cpu.total);
    let share = |a: u64, b: u64| -> f32 {
        if rebooted || clock_regressed || aggregate_cpu_regressed || cpu_total == 0 {
            return 0.0;
        }
        // saturating_sub, not a checked subtraction: /proc/stat's per-class counters can go
        // backwards by a jiffy or two across a CPU hotplug, and a negative percentage would fail
        // the control plane's CHECK and cost the whole round.
        (a.saturating_sub(b) as f32 / cpu_total as f32) * 100.0
    };
    let delta = |a: u64, b: u64| -> u64 {
        if invalid_interval {
            0
        } else {
            a.saturating_sub(b)
        }
    };
    let optional_delta = |a: Option<u64>, b: Option<u64>| -> Option<u64> {
        match (a, b) {
            (Some(a), Some(b)) => Some(if invalid_interval {
                0
            } else {
                a.saturating_sub(b)
            }),
            _ => None,
        }
    };
    let pressure_pct = |a: Option<u64>, b: Option<u64>| -> Option<f32> {
        let stalled = optional_delta(a, b)?;
        Some(((stalled as f64 / (elapsed as f64 * 1_000_000.0)) * 100.0).min(100.0) as f32)
    };

    let cpu_cores = now
        .cpu_cores
        .iter()
        .filter_map(|(cpu, current)| {
            let previous = before.cpu_cores.get(cpu)?;
            if cpu_times_regressed(*previous, *current) {
                return None;
            }
            let total = current.total.saturating_sub(previous.total);
            let part = |a: u64, b: u64| -> f32 {
                if invalid_interval || total == 0 {
                    0.0
                } else {
                    ((a.saturating_sub(b) as f64 / total as f64) * 100.0).min(100.0) as f32
                }
            };
            Some(CpuCoreSample {
                cpu: *cpu,
                user_pct: part(current.user, previous.user),
                system_pct: part(current.system, previous.system),
                softirq_pct: part(current.softirq, previous.softirq),
                iowait_pct: part(current.iowait, previous.iowait),
                steal_pct: part(current.steal, previous.steal),
            })
        })
        .collect();

    let softirq_rate = |select: fn(SoftirqCounters) -> u64| -> Option<u64> {
        let current = select(now.softirqs?);
        let previous = select(before.softirqs?);
        Some(delta(current, previous) / elapsed)
    };
    let gup_pinned_bytes = match (now.vm.foll_pin_acquired, now.vm.foll_pin_released) {
        (Some(acquired), Some(released)) => Some(
            acquired
                .saturating_sub(released)
                .saturating_mul(page_size()),
        ),
        _ => None,
    };
    let disk = match (before.disk, now.disk) {
        (Some(before), Some(now))
            if !invalid_interval
                && before.major == now.major
                && before.minor == now.minor
                && now.reads >= before.reads
                && now.sectors_read >= before.sectors_read
                && now.read_ms >= before.read_ms
                && now.writes >= before.writes
                && now.sectors_written >= before.sectors_written
                && now.write_ms >= before.write_ms
                && now.busy_ms >= before.busy_ms
                && now.weighted_ms >= before.weighted_ms =>
        {
            // Linux diskstats sectors are always 512-byte accounting units, regardless of the
            // device's physical/logical sector size (Documentation/admin-guide/iostats.rst).
            Some(DiskDelta {
                read_bytes: (now.sectors_read - before.sectors_read).saturating_mul(512),
                write_bytes: (now.sectors_written - before.sectors_written).saturating_mul(512),
                reads: now.reads - before.reads,
                writes: now.writes - before.writes,
                read_ms: now.read_ms - before.read_ms,
                write_ms: now.write_ms - before.write_ms,
                in_flight: now.in_flight,
                busy_ms: now.busy_ms - before.busy_ms,
                weighted_ms: now.weighted_ms - before.weighted_ms,
            })
        }
        _ => None,
    };
    let network_detail = now.network.has_any().then(|| NetworkDetailSample {
        // Levels belong to the end of this sub-window; counters are differences over it.
        tcp_curr_estab: now.network.tcp_curr_estab,
        tcp_inuse: now.network.tcp_inuse,
        tcp_time_wait: now.network.tcp_time_wait,
        tcp_orphan: now.network.tcp_orphan,
        tcp_alloc: now.network.tcp_alloc,
        tcp_mem_bytes: now
            .network
            .tcp_mem_pages
            .map(|pages| pages.saturating_mul(page_size())),
        udp_inuse: now.network.udp_inuse,
        udp_mem_bytes: now
            .network
            .udp_mem_pages
            .map(|pages| pages.saturating_mul(page_size())),
        // Filled from the all-state inet_diag snapshot once per finished 30-second window. Doing
        // that O(number of sockets) walk on every 10-second sub-sample would triple its cost.
        ephemeral_port_capacity: None,
        tcp_ephemeral_inuse_v4: None,
        tcp_ephemeral_inuse_v6: None,
        tcp_ephemeral_time_wait_v4: None,
        tcp_ephemeral_time_wait_v6: None,
        tcp_ephemeral_top_target_v4: None,
        tcp_ephemeral_top_target_v6: None,
        tcp_active_opens: optional_delta(
            now.network.tcp_active_opens,
            before.network.tcp_active_opens,
        ),
        tcp_passive_opens: optional_delta(
            now.network.tcp_passive_opens,
            before.network.tcp_passive_opens,
        ),
        tcp_attempt_fails: optional_delta(
            now.network.tcp_attempt_fails,
            before.network.tcp_attempt_fails,
        ),
        tcp_estab_resets: optional_delta(
            now.network.tcp_estab_resets,
            before.network.tcp_estab_resets,
        ),
        tcp_retrans_segs: optional_delta(
            now.network.tcp_retrans_segs,
            before.network.tcp_retrans_segs,
        ),
        tcp_syn_retrans: optional_delta(
            now.network.tcp_syn_retrans,
            before.network.tcp_syn_retrans,
        ),
        tcp_in_errors: optional_delta(now.network.tcp_in_errors, before.network.tcp_in_errors),
        tcp_out_resets: optional_delta(now.network.tcp_out_resets, before.network.tcp_out_resets),
        tcp_timeouts: optional_delta(now.network.tcp_timeouts, before.network.tcp_timeouts),
        tcp_listen_overflows: optional_delta(
            now.network.tcp_listen_overflows,
            before.network.tcp_listen_overflows,
        ),
        tcp_listen_drops: optional_delta(
            now.network.tcp_listen_drops,
            before.network.tcp_listen_drops,
        ),
        udp_in_errors: optional_delta(now.network.udp_in_errors, before.network.udp_in_errors),
        udp_no_ports: optional_delta(now.network.udp_no_ports, before.network.udp_no_ports),
        udp_rcvbuf_errors: optional_delta(
            now.network.udp_rcvbuf_errors,
            before.network.udp_rcvbuf_errors,
        ),
        udp_sndbuf_errors: optional_delta(
            now.network.udp_sndbuf_errors,
            before.network.udp_sndbuf_errors,
        ),
    });

    let mut proc_cpu_pct = BTreeMap::new();
    for (name, jiffies) in &now.proc_cpu {
        let before_j = before.proc_cpu.get(name).copied().unwrap_or(0);
        // A process that restarted inside the window has its own counter reset; its jiffies going
        // backwards is that, not negative CPU.
        if invalid_interval || aggregate_cpu_regressed || *jiffies < before_j || cpu_total == 0 {
            proc_cpu_pct.insert(name.clone(), 0.0);
            continue;
        }
        proc_cpu_pct.insert(
            name.clone(),
            ((jiffies - before_j) as f32 / cpu_total as f32) * 100.0,
        );
    }

    Sub {
        window_start_unix_secs: before.at,
        window_end_unix_secs: now.at,
        elapsed_secs: elapsed,
        cpu_user_pct: share(now.cpu.user, before.cpu.user),
        cpu_sys_pct: share(now.cpu.system, before.cpu.system),
        cpu_softirq_pct: share(now.cpu.softirq, before.cpu.softirq),
        cpu_iowait_pct: share(now.cpu.iowait, before.cpu.iowait),
        cpu_steal_pct: share(now.cpu.steal, before.cpu.steal),
        cpu_cores,
        cpu_pressure_some_pct: pressure_pct(
            now.cpu_pressure.some_usec,
            before.cpu_pressure.some_usec,
        ),
        io_pressure_some_pct: pressure_pct(now.io_pressure.some_usec, before.io_pressure.some_usec),
        io_pressure_full_pct: pressure_pct(now.io_pressure.full_usec, before.io_pressure.full_usec),
        context_switches_per_sec: optional_delta(now.context_switches, before.context_switches)
            .map(|v| v / elapsed),
        net_rx_softirqs_per_sec: softirq_rate(|s| s.net_rx),
        net_tx_softirqs_per_sec: softirq_rate(|s| s.net_tx),
        cpu_throttled_usec: optional_delta(now.cpu_throttled_usec, before.cpu_throttled_usec),
        nic_rx_bps: delta(now.net.rx_bytes, before.net.rx_bytes) * 8 / elapsed,
        nic_tx_bps: delta(now.net.tx_bytes, before.net.tx_bytes) * 8 / elapsed,
        nic_rx_drop: delta(now.net.rx_drop, before.net.rx_drop),
        nic_tx_drop: delta(now.net.tx_drop, before.net.tx_drop),
        nic_err: delta(now.net.errs, before.net.errs),
        network_detail,
        oom_kills: delta(now.oom_kills, before.oom_kills),
        memory: now.memory.clone(),
        memory_pressure_some_pct: pressure_pct(
            now.memory_pressure.some_usec,
            before.memory_pressure.some_usec,
        ),
        memory_pressure_full_pct: pressure_pct(
            now.memory_pressure.full_usec,
            before.memory_pressure.full_usec,
        ),
        swap_in_bytes: delta(now.vm.swap_in_pages, before.vm.swap_in_pages)
            .saturating_mul(page_size()),
        swap_out_bytes: delta(now.vm.swap_out_pages, before.vm.swap_out_pages)
            .saturating_mul(page_size()),
        major_faults: delta(now.vm.major_faults, before.vm.major_faults),
        direct_reclaim_pages: delta(now.vm.direct_reclaim_pages, before.vm.direct_reclaim_pages),
        gup_pinned_bytes,
        disk,
        proc_cpu_pct,
        gap: rebooted
            || clock_regressed
            || aggregate_cpu_regressed
            || core_cpu_regressed
            || delayed,
    }
}

fn cpu_times_regressed(before: CpuTimes, now: CpuTimes) -> bool {
    now.user < before.user
        || now.system < before.system
        || now.softirq < before.softirq
        || now.iowait < before.iowait
        || now.steal < before.steal
        || now.total < before.total
}

/// btime is not on the sample: it is reported once per round, on `LoadReportRequest`. A window
/// cannot disagree with its own report about when the machine booted.
fn aggregate(subs: &[Sub], state_dir: &Path) -> LoadSample {
    let mean = |f: fn(&Sub) -> f32| weighted_f32(subs, f);
    let user = mean(|s| s.cpu_user_pct);
    let sys = mean(|s| s.cpu_sys_pct);
    let softirq = mean(|s| s.cpu_softirq_pct);
    let mean_optional = |f: fn(&Sub) -> Option<f32>| weighted_f32_option(subs, f);
    // The peak is over the *sum* per sub-sample, not the sum of per-class peaks: the latter adds
    // three maxima that never occurred together and can exceed 100 on a machine that was never
    // saturated.
    let peak = subs
        .iter()
        .map(|s| s.cpu_user_pct + s.cpu_sys_pct + s.cpu_softirq_pct)
        .fold(0.0_f32, f32::max);

    let mem = subs.last().map(|s| s.memory.clone()).unwrap_or_default();
    let available_min = subs
        .iter()
        .map(|s| s.memory.available)
        .min()
        .unwrap_or(mem.available);
    let load = read_loadavg();
    let mut cores = BTreeMap::<u32, Vec<(&CpuCoreSample, u64)>>::new();
    for sub in subs {
        for core in &sub.cpu_cores {
            cores
                .entry(core.cpu)
                .or_default()
                .push((core, sub.elapsed_secs));
        }
    }
    let cores = cores
        .into_iter()
        .filter(|(_, readings)| readings.len() == subs.len())
        .map(|(cpu, readings)| {
            let mean_core = |f: fn(&CpuCoreSample) -> f32| {
                let total = readings
                    .iter()
                    .map(|(_, elapsed)| *elapsed)
                    .sum::<u64>()
                    .max(1);
                readings
                    .iter()
                    .map(|(reading, elapsed)| f(reading) as f64 * *elapsed as f64)
                    .sum::<f64>() as f32
                    / total as f32
            };
            CpuCoreSample {
                cpu,
                user_pct: mean_core(|c| c.user_pct),
                system_pct: mean_core(|c| c.system_pct),
                softirq_pct: mean_core(|c| c.softirq_pct),
                iowait_pct: mean_core(|c| c.iowait_pct),
                steal_pct: mean_core(|c| c.steal_pct),
            }
        })
        .collect();
    let kernel_other_bytes = mem.total.saturating_sub(
        mem.free
            .saturating_add(mem.anon)
            .saturating_add(mem.file_cache)
            .saturating_add(mem.shmem),
    );
    let disk = read_disk(state_dir);
    let window_start = subs
        .first()
        .map(|sub| sub.window_start_unix_secs)
        .unwrap_or(0);
    let window_end = subs
        .last()
        .map(|sub| sub.window_end_unix_secs)
        .unwrap_or(window_start);
    let network_detail = subs
        .last()
        .and_then(|sub| sub.network_detail.as_ref())
        .map(|last| NetworkDetailSample {
            // Socket inventory is a level, so the last sub-sample is the honest value for the
            // window. Event counters are deltas and therefore add across all three sub-samples.
            tcp_curr_estab: last.tcp_curr_estab,
            tcp_inuse: last.tcp_inuse,
            tcp_time_wait: last.tcp_time_wait,
            tcp_orphan: last.tcp_orphan,
            tcp_alloc: last.tcp_alloc,
            tcp_mem_bytes: last.tcp_mem_bytes,
            udp_inuse: last.udp_inuse,
            udp_mem_bytes: last.udp_mem_bytes,
            ephemeral_port_capacity: last.ephemeral_port_capacity,
            tcp_ephemeral_inuse_v4: last.tcp_ephemeral_inuse_v4,
            tcp_ephemeral_inuse_v6: last.tcp_ephemeral_inuse_v6,
            tcp_ephemeral_time_wait_v4: last.tcp_ephemeral_time_wait_v4,
            tcp_ephemeral_time_wait_v6: last.tcp_ephemeral_time_wait_v6,
            tcp_ephemeral_top_target_v4: last.tcp_ephemeral_top_target_v4,
            tcp_ephemeral_top_target_v6: last.tcp_ephemeral_top_target_v6,
            tcp_active_opens: sum_u64_option(subs, |s| s.network_detail.as_ref()?.tcp_active_opens),
            tcp_passive_opens: sum_u64_option(subs, |s| {
                s.network_detail.as_ref()?.tcp_passive_opens
            }),
            tcp_attempt_fails: sum_u64_option(subs, |s| {
                s.network_detail.as_ref()?.tcp_attempt_fails
            }),
            tcp_estab_resets: sum_u64_option(subs, |s| s.network_detail.as_ref()?.tcp_estab_resets),
            tcp_retrans_segs: sum_u64_option(subs, |s| s.network_detail.as_ref()?.tcp_retrans_segs),
            tcp_syn_retrans: sum_u64_option(subs, |s| s.network_detail.as_ref()?.tcp_syn_retrans),
            tcp_in_errors: sum_u64_option(subs, |s| s.network_detail.as_ref()?.tcp_in_errors),
            tcp_out_resets: sum_u64_option(subs, |s| s.network_detail.as_ref()?.tcp_out_resets),
            tcp_timeouts: sum_u64_option(subs, |s| s.network_detail.as_ref()?.tcp_timeouts),
            tcp_listen_overflows: sum_u64_option(subs, |s| {
                s.network_detail.as_ref()?.tcp_listen_overflows
            }),
            tcp_listen_drops: sum_u64_option(subs, |s| s.network_detail.as_ref()?.tcp_listen_drops),
            udp_in_errors: sum_u64_option(subs, |s| s.network_detail.as_ref()?.udp_in_errors),
            udp_no_ports: sum_u64_option(subs, |s| s.network_detail.as_ref()?.udp_no_ports),
            udp_rcvbuf_errors: sum_u64_option(subs, |s| {
                s.network_detail.as_ref()?.udp_rcvbuf_errors
            }),
            udp_sndbuf_errors: sum_u64_option(subs, |s| {
                s.network_detail.as_ref()?.udp_sndbuf_errors
            }),
        });
    let elapsed = elapsed_total(subs);
    let disk_deltas = subs.iter().filter_map(|sub| sub.disk).collect::<Vec<_>>();
    let disk_detail = {
        let sum = |read: fn(DiskDelta) -> u64| disk_deltas.iter().copied().map(read).sum::<u64>();
        let reads = sum(|d| d.reads);
        let writes = sum(|d| d.writes);
        let read_ms = sum(|d| d.read_ms);
        let write_ms = sum(|d| d.write_ms);
        DiskDetailSample {
            total_bytes: (disk.total_bytes > 0).then_some(disk.total_bytes),
            inode_total: disk.inode_total,
            inode_free: disk.inode_free,
            read_bps: (disk_deltas.len() == subs.len()).then(|| sum(|d| d.read_bytes) / elapsed),
            write_bps: (disk_deltas.len() == subs.len()).then(|| sum(|d| d.write_bytes) / elapsed),
            read_iops: (disk_deltas.len() == subs.len()).then(|| reads as f32 / elapsed as f32),
            write_iops: (disk_deltas.len() == subs.len()).then(|| writes as f32 / elapsed as f32),
            read_await_ms: (disk_deltas.len() == subs.len() && reads > 0)
                .then(|| read_ms as f32 / reads as f32),
            write_await_ms: (disk_deltas.len() == subs.len() && writes > 0)
                .then(|| write_ms as f32 / writes as f32),
            busy_pct: (disk_deltas.len() == subs.len()).then(|| {
                (sum(|d| d.busy_ms) as f64 / (elapsed as f64 * 1_000.0) * 100.0).min(100.0) as f32
            }),
            queue_depth: (disk_deltas.len() == subs.len())
                .then(|| (sum(|d| d.weighted_ms) as f64 / (elapsed as f64 * 1_000.0)) as f32),
            in_flight: disk_deltas.last().map(|d| d.in_flight),
            pressure_some_pct: mean_optional(|s| s.io_pressure_some_pct),
            pressure_full_pct: mean_optional(|s| s.io_pressure_full_pct),
        }
    };

    LoadSample {
        window_start_unix_secs: window_start as i64,
        window_end_unix_secs: window_end as i64,
        has_gap: subs.iter().any(|s| s.gap),
        cpu_user_pct: user,
        cpu_sys_pct: sys,
        cpu_softirq_pct: softirq,
        cpu_peak_pct: peak.max(user + sys + softirq).min(100.0),
        cpu_steal_pct: mean(|s| s.cpu_steal_pct),
        load1: load.one,
        cpu_detail: Some(CpuDetailSample {
            iowait_pct: mean(|s| s.cpu_iowait_pct),
            load5: load.five,
            load15: load.fifteen,
            pressure_some_pct: mean_optional(|s| s.cpu_pressure_some_pct),
            io_pressure_some_pct: mean_optional(|s| s.io_pressure_some_pct),
            io_pressure_full_pct: mean_optional(|s| s.io_pressure_full_pct),
            procs_running: load.procs_running,
            procs_total: load.procs_total,
            context_switches_per_sec: mean_u64_option(subs, |s| s.context_switches_per_sec),
            net_rx_softirqs_per_sec: mean_u64_option(subs, |s| s.net_rx_softirqs_per_sec),
            net_tx_softirqs_per_sec: mean_u64_option(subs, |s| s.net_tx_softirqs_per_sec),
            throttled_usec: sum_u64_option(subs, |s| s.cpu_throttled_usec),
            frequency_mhz: read_cpu_frequency_mhz("scaling_cur_freq"),
            cores,
        }),
        mem_available_bytes: mem.available,
        swap_used_bytes: mem.swap_used,
        memory_detail: Some(MemoryDetailSample {
            available_min_bytes: available_min,
            free_bytes: mem.free,
            anon_bytes: mem.anon,
            file_cache_bytes: mem.file_cache,
            shmem_bytes: mem.shmem,
            kernel_other_bytes,
            buffers_bytes: mem.buffers,
            kernel_reclaimable_bytes: mem.kernel_reclaimable,
            slab_unreclaimable_bytes: mem.slab_unreclaimable,
            unevictable_bytes: mem.unevictable,
            mlocked_bytes: mem.mlocked,
            dirty_bytes: mem.dirty,
            writeback_bytes: mem.writeback,
            swap_total_bytes: mem.swap_total,
            swap_cached_bytes: mem.swap_cached,
            zswap_bytes: mem.zswap,
            zswapped_bytes: mem.zswapped,
            gup_pinned_bytes: subs.last().and_then(|s| s.gup_pinned_bytes),
            swap_in_bytes: subs.iter().map(|s| s.swap_in_bytes).sum(),
            swap_out_bytes: subs.iter().map(|s| s.swap_out_bytes).sum(),
            pressure_some_pct: mean_optional(|s| s.memory_pressure_some_pct),
            pressure_full_pct: mean_optional(|s| s.memory_pressure_full_pct),
            major_faults: subs.iter().map(|s| s.major_faults).sum(),
            direct_reclaim_pages: subs.iter().map(|s| s.direct_reclaim_pages).sum(),
        }),
        oom_kills: subs.iter().map(|s| s.oom_kills).sum(),
        disk_free_bytes: disk.free_bytes,
        disk_inode_free_pct: disk.inode_free_pct,
        // Every new Agent supplies the object even when an overlay filesystem has no block row:
        // capacity/inode history remains expandable and the unavailable device fields stay null.
        disk_detail: Some(disk_detail),
        // Rates are averaged over the window; drops and errors are summed, because they are events
        // rather than levels — "3 packets dropped in this window" is the fact, and averaging it to
        // one per sub-sample loses the count.
        nic_rx_bps: weighted_u64(subs, |s| s.nic_rx_bps),
        nic_tx_bps: weighted_u64(subs, |s| s.nic_tx_bps),
        nic_rx_drop: subs.iter().map(|s| s.nic_rx_drop).sum(),
        nic_tx_drop: subs.iter().map(|s| s.nic_tx_drop).sum(),
        nic_err: subs.iter().map(|s| s.nic_err).sum(),
        conntrack_count: read_u64_file("/proc/sys/net/netfilter/nf_conntrack_count"),
        network_detail,
        uptime_secs: read_uptime(),
    }
}

fn elapsed_total(subs: &[Sub]) -> u64 {
    subs.iter().map(|sub| sub.elapsed_secs).sum::<u64>().max(1)
}

fn weighted_f32(subs: &[Sub], f: fn(&Sub) -> f32) -> f32 {
    let total = elapsed_total(subs);
    (subs
        .iter()
        .map(|sub| f(sub) as f64 * sub.elapsed_secs as f64)
        .sum::<f64>()
        / total as f64) as f32
}

fn weighted_f32_option(subs: &[Sub], f: fn(&Sub) -> Option<f32>) -> Option<f32> {
    let mut sum = 0.0_f64;
    for sub in subs {
        sum += f(sub)? as f64 * sub.elapsed_secs as f64;
    }
    Some((sum / elapsed_total(subs) as f64) as f32)
}

fn weighted_u64(subs: &[Sub], f: fn(&Sub) -> u64) -> u64 {
    let sum = subs
        .iter()
        .map(|sub| u128::from(f(sub)) * u128::from(sub.elapsed_secs))
        .sum::<u128>();
    u64::try_from(sum / u128::from(elapsed_total(subs))).unwrap_or(u64::MAX)
}

fn mean_u64_option(subs: &[Sub], f: fn(&Sub) -> Option<u64>) -> Option<u64> {
    let mut sum = 0_u128;
    for sub in subs {
        sum = sum.saturating_add(u128::from(f(sub)?) * u128::from(sub.elapsed_secs));
    }
    Some(u64::try_from(sum / u128::from(elapsed_total(subs))).unwrap_or(u64::MAX))
}

fn sum_u64_option(subs: &[Sub], f: fn(&Sub) -> Option<u64>) -> Option<u64> {
    let values: Vec<u64> = subs.iter().filter_map(f).collect();
    (values.len() == subs.len() && !values.is_empty()).then(|| values.iter().sum())
}

/// The four processes we put on the machine.
///
/// wg is reported with everything `None` when the backend is in-kernel, which is honest rather
/// than lazy: WireGuard has no process then, its cost lands in softirq, and inventing a number for
/// it would be inventing a number. On a userspace backend (wireguard-go / boringtun) there is a
/// real process and it is measured like any other.
fn processes(subs: &[Sub], raw: &Raw) -> Vec<ProcessSample> {
    let cpu_of = |name: &str| -> Option<f32> {
        let sum: f64 = subs
            .iter()
            .map(|s| {
                s.proc_cpu_pct.get(name).copied().unwrap_or(0.0) as f64 * s.elapsed_secs as f64
            })
            .sum();
        raw.proc_cpu
            .contains_key(name)
            .then_some((sum / elapsed_total(subs) as f64) as f32)
    };

    let mut out = Vec::with_capacity(4);
    for (label, comm) in [
        ("xray", "xray"),
        ("phantun", "phantun-client"),
        ("agent", "brocade-agent"),
    ] {
        // From the walk read_raw already did — not a fresh one.
        let pid = raw.pids.get(comm).copied();
        out.push(match pid {
            Some(pid) => ProcessSample {
                proc: label.to_owned(),
                rss_bytes: read_rss(pid),
                cpu_pct: cpu_of(comm),
                started_at_unix_secs: read_started_at(pid, raw.btime),
                fds: count_fds(pid),
                fd_limit: read_fd_limit(pid),
            },
            None => ProcessSample {
                proc: label.to_owned(),
                rss_bytes: None,
                cpu_pct: None,
                started_at_unix_secs: None,
                fds: None,
                fd_limit: None,
            },
        });
    }

    let userspace_wg = raw
        .pids
        .get("wireguard-go")
        .or_else(|| raw.pids.get("boringtun"))
        .copied();
    out.push(match userspace_wg {
        Some(pid) => ProcessSample {
            proc: "wg".to_owned(),
            rss_bytes: read_rss(pid),
            cpu_pct: None,
            started_at_unix_secs: read_started_at(pid, raw.btime),
            fds: count_fds(pid),
            fd_limit: read_fd_limit(pid),
        },
        None => ProcessSample {
            proc: "wg".to_owned(),
            rss_bytes: None,
            cpu_pct: None,
            started_at_unix_secs: None,
            fds: None,
            fd_limit: None,
        },
    });
    out
}

/// What changes slowly. Read once per report rather than per sub-sample.
pub(crate) fn host_facts(state_dir: &Path) -> HostFacts {
    let detected_nic = main_interface();
    // Keep the historical display/qdisc fallback, but do not present eth0's MTU as the default
    // route's MTU when route discovery itself failed.
    let nic_mtu = detected_nic.as_deref().and_then(read_nic_mtu);
    let nic = detected_nic.unwrap_or_else(|| "eth0".to_owned());
    let mem = read_meminfo();
    let disk = read_disk(state_dir);
    let disk_identity = read_disk_identity(state_dir);
    let ephemeral = read_ephemeral_port_range();
    HostFacts {
        kernel: read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_default(),
        cpu_model: cpu_model(),
        cores: num_cores(),
        cpu_freq_max_mhz: read_cpu_max_frequency_mhz(),
        cpu_governor: read_cpu_governor(),
        cc_algo: read_trimmed("/proc/sys/net/ipv4/tcp_congestion_control").unwrap_or_default(),
        available_cc: read_trimmed("/proc/sys/net/ipv4/tcp_available_congestion_control")
            .map(|s| s.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default(),
        default_qdisc: read_trimmed("/proc/sys/net/core/default_qdisc").unwrap_or_default(),
        nic_qdisc: nic_qdisc(&nic),
        nic,
        nic_mtu,
        mem_total_bytes: mem.total,
        disk_total_bytes: disk.total_bytes,
        disk_mount: disk_identity.as_ref().map(|value| value.mount.clone()),
        disk_filesystem: disk_identity.as_ref().map(|value| value.filesystem.clone()),
        disk_device: disk_identity.as_ref().map(|value| value.device.clone()),
        disk_read_only: disk_identity.as_ref().map(|value| value.read_only),
        conntrack_max: read_u64_file("/proc/sys/net/netfilter/nf_conntrack_max"),
        ephemeral_port_low: ephemeral.as_ref().map(|value| value.low),
        ephemeral_port_high: ephemeral.as_ref().map(|value| value.high),
        ephemeral_port_capacity: ephemeral.as_ref().map(|value| value.capacity),
        sysctl_managed: installer_set_congestion_control(),
        arch: std::env::consts::ARCH.to_owned(),
        os_pretty: os_pretty_name(),
        virt: detect_virt(),
        rmem_max: read_u64_file("/proc/sys/net/core/rmem_max").unwrap_or(0),
        wmem_max: read_u64_file("/proc/sys/net/core/wmem_max").unwrap_or(0),
        somaxconn: read_u64_file("/proc/sys/net/core/somaxconn").unwrap_or(0),
    }
}

/// Processor model without making `/proc/cpuinfo`'s architecture-specific spelling part of the
/// wire protocol. x86 reports `model name`; many arm64 guests expose only an implementer/part
/// pair, so the common server parts are translated and unknown pairs remain identifiable rather
/// than becoming an empty field.
fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .map(|text| parse_cpu_model(&text))
        .unwrap_or_default()
}

fn parse_cpu_model(text: &str) -> String {
    if let Some(value) = cpuinfo_value(text, "model name") {
        return normalize_cpu_model(value);
    }

    let implementer = cpuinfo_value(text, "CPU implementer");
    let part = cpuinfo_value(text, "CPU part");
    if let (Some(implementer), Some(part)) = (implementer, part) {
        if let Some(name) = arm_part_name(implementer, part) {
            return name.to_owned();
        }
    }

    // Hardware is generally the SoC/board model and Processor is often the generic
    // "AArch64 Processor rev …". Both are still more useful than a raw MIDR pair, but a known
    // implementer/part mapping above is the actual CPU model and therefore wins.
    for key in ["Hardware", "Processor"] {
        if let Some(value) = cpuinfo_value(text, key) {
            return normalize_cpu_model(value);
        }
    }

    match (implementer, part) {
        (Some(implementer), Some(part)) => format!("ARM {}:{}", implementer.trim(), part.trim()),
        _ => String::new(),
    }
}

fn cpuinfo_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name.trim() == key && !value.trim().is_empty()).then(|| value.trim())
    })
}

fn normalize_cpu_model(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn arm_part_name(implementer: &str, part: &str) -> Option<&'static str> {
    let hex = |value: &str| u32::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok();
    match (hex(implementer)?, hex(part)?) {
        (0x41, 0xd03) => Some("Cortex-A53"),
        (0x41, 0xd04) => Some("Cortex-A35"),
        (0x41, 0xd05) => Some("Cortex-A55"),
        (0x41, 0xd07) => Some("Cortex-A57"),
        (0x41, 0xd08) => Some("Cortex-A72"),
        (0x41, 0xd09) => Some("Cortex-A73"),
        (0x41, 0xd0a) => Some("Cortex-A75"),
        (0x41, 0xd0b) => Some("Cortex-A76"),
        (0x41, 0xd0c) => Some("Neoverse-N1"),
        (0x41, 0xd0d) => Some("Cortex-A77"),
        (0x41, 0xd40) => Some("Neoverse-V1"),
        (0x41, 0xd41) => Some("Cortex-A78"),
        (0x41, 0xd44) => Some("Cortex-X1"),
        (0x41, 0xd46) => Some("Cortex-A510"),
        (0x41, 0xd47) => Some("Cortex-A710"),
        (0x41, 0xd48) => Some("Cortex-X2"),
        (0x41, 0xd49) => Some("Neoverse-N2"),
        (0x41, 0xd4d) => Some("Cortex-A715"),
        (0x41, 0xd4e) => Some("Cortex-X3"),
        (0x41, 0xd4f) => Some("Neoverse-V2"),
        (0x43, 0x0af) => Some("Cavium ThunderX2"),
        _ => None,
    }
}

/// `PRETTY_NAME` from `/etc/os-release`. Empty when the file is missing, which minimal images
/// and some containers are — the UI shows a dash for empty rather than a half-guessed name.
fn os_pretty_name() -> String {
    let Ok(text) = fs::read_to_string("/etc/os-release") else {
        return String::new();
    };
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .unwrap_or_default()
}

/// The virtualization platform's short name, without shelling out to `systemd-detect-virt` (not
/// necessarily installed). Three sources cover the fleet: `/run/systemd/container` (containers),
/// `/proc/vz` (OpenVZ), and the DMI product name (VMs). A bare-metal machine's DMI string is its
/// mainboard model; anything not in the mapping returns empty rather than displaying that model
/// as if it were a platform.
fn detect_virt() -> String {
    if let Ok(c) = fs::read_to_string("/run/systemd/container") {
        let c = c.trim();
        if !c.is_empty() {
            return c.to_owned();
        }
    }
    if Path::new("/proc/vz").exists() {
        return "OpenVZ".to_owned();
    }
    let Ok(dmi) = fs::read_to_string("/sys/class/dmi/id/product_name") else {
        return String::new();
    };
    let lower = dmi.to_lowercase();
    let name = if lower.contains("kvm") || lower.contains("qemu") {
        "KVM"
    } else if lower.contains("vmware") {
        "VMware"
    } else if lower.contains("virtualbox") {
        "VirtualBox"
    } else if lower.contains("microsoft") || lower.contains("hyper-v") {
        "Hyper-V"
    } else if lower.contains("xen") {
        "Xen"
    } else if lower.contains("amazon") || lower.contains("ec2") {
        "AWS"
    } else if lower.contains("google") {
        "GCP"
    } else if lower.contains("oracle") {
        "Oracle Cloud"
    } else if lower.contains("alibaba") || lower.contains("aliyun") {
        "Alibaba Cloud"
    } else if lower.contains("tencent") {
        "Tencent Cloud"
    } else if lower.contains("digitalocean") {
        "DigitalOcean"
    } else {
        ""
    };
    name.to_owned()
}

/// Whether the installer set the congestion control algorithm on this machine.
///
/// The test is the key inside `99-brocade.conf`, not the file's existence. That file carries two
/// unrelated blocks now: `install.sh`'s `tune_congestion` deletes the whole file when bbr did not
/// take (OpenVZ and friends, where sysctl is read-only), and `tune_conntrack` runs afterwards and
/// writes it again with only the conntrack sizing — deliberately, because those are exactly the
/// machines whose connection table fills first. Testing existence would then report bbr as
/// configured on a machine where it demonstrably is not, and the console's conclusion built on
/// this field — "the installer set bbr and something changed it back" — would be a fabrication.
fn installer_set_congestion_control() -> bool {
    fs::read_to_string("/etc/sysctl.d/99-brocade.conf")
        .is_ok_and(|text| text.contains("tcp_congestion_control"))
}

/// What is actually attached to the interface, which is not what `net.core.default_qdisc` says.
///
/// Changing the default only affects queues created afterwards; an interface that is already up
/// keeps whatever it had. Reading back only the default therefore reports success on a machine
/// where nothing changed — which is precisely the failure this field exists to catch, so it has to
/// come from the interface itself.
///
/// This is the one place here that shells out. `tc` speaks netlink and doing it ourselves is
/// perfectly possible, but unlike the ICMP case there is no measurement to get wrong: either the
/// string parses or the field is empty, and an empty field costs one diagnostic rather than a
/// wrong number. If `tc` turns out to be missing often enough to matter, this is the thing to
/// replace with RTM_GETQDISC.
fn nic_qdisc(nic: &str) -> String {
    let Ok(output) = run_command("tc", &["qdisc", "show", "dev", nic]) else {
        return String::new();
    };
    // `qdisc fq 8001: root refcnt 2 limit 10000p ...`
    output
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned()
}

/// The actual layer-3 MTU on the interface carrying the default route.
///
/// This is deliberately read from sysfs rather than inferred from the configured wg0 MTU or from
/// path probing: those values describe different layers and can legitimately be smaller.
fn read_nic_mtu(nic: &str) -> Option<u32> {
    fs::read_to_string(Path::new("/sys/class/net").join(nic).join("mtu"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn read_raw(now: u64, state_dir: &Path) -> Raw {
    let stat = fs::read_to_string("/proc/stat").unwrap_or_default();
    let vm = read_vmstat();
    let pids = find_pids();
    Raw {
        at: now,
        btime: parse_btime(&stat).unwrap_or(0),
        cpu: parse_cpu(&stat),
        cpu_cores: parse_cpu_cores(&stat),
        context_switches: parse_stat_counter(&stat, "ctxt "),
        cpu_pressure: read_pressure("/proc/pressure/cpu"),
        softirqs: read_softirqs(),
        cpu_throttled_usec: read_cpu_throttled_usec(),
        net: read_net(),
        network: read_network(),
        oom_kills: vm.oom_kills,
        memory: read_meminfo(),
        vm,
        memory_pressure: read_pressure("/proc/pressure/memory"),
        io_pressure: read_pressure("/proc/pressure/io"),
        disk: read_disk_counters(state_dir),
        proc_cpu: read_proc_cpu(&pids),
        pids,
    }
}

fn parse_cpu(stat: &str) -> CpuTimes {
    // The aggregate line: `cpu  user nice system idle iowait irq softirq steal guest guest_nice`
    stat.lines()
        .find(|line| line.starts_with("cpu "))
        .and_then(parse_cpu_line)
        .unwrap_or_default()
}

fn parse_cpu_cores(stat: &str) -> BTreeMap<u32, CpuTimes> {
    stat.lines()
        .filter_map(|line| {
            let label = line.split_whitespace().next()?;
            let cpu = label.strip_prefix("cpu")?.parse::<u32>().ok()?;
            Some((cpu, parse_cpu_line(line)?))
        })
        .collect()
}

fn parse_cpu_line(line: &str) -> Option<CpuTimes> {
    let f: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    if f.is_empty() {
        return None;
    }
    let get = |i: usize| f.get(i).copied().unwrap_or(0);
    Some(CpuTimes {
        // nice folded into user: it is user time, and splitting it out would add a fourth class to
        // a display whose whole point is three.
        user: get(0) + get(1),
        // irq folded into system, softirq kept apart. That asymmetry is the point of this split:
        // softirq is where packet forwarding lands, and it is the one class whose growth changes
        // what an operator should do.
        system: get(2) + get(5),
        softirq: get(6),
        iowait: get(4),
        steal: get(7),
        // guest and guest_nice are already included in user/nice and would be double-counted.
        total: f.iter().take(8).sum(),
    })
}

fn parse_stat_counter(stat: &str, key: &str) -> Option<u64> {
    stat.lines()
        .find_map(|line| line.strip_prefix(key))?
        .trim()
        .parse()
        .ok()
}

fn parse_btime(stat: &str) -> Option<i64> {
    stat.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse().ok())
}

fn read_btime() -> Option<i64> {
    parse_btime(&fs::read_to_string("/proc/stat").ok()?)
}

#[derive(Clone, Default)]
struct MemInfo {
    total: u64,
    available: u64,
    free: u64,
    anon: u64,
    file_cache: u64,
    shmem: u64,
    buffers: u64,
    kernel_reclaimable: u64,
    slab_unreclaimable: u64,
    unevictable: u64,
    mlocked: u64,
    dirty: u64,
    writeback: u64,
    swap_total: u64,
    swap_used: u64,
    swap_cached: u64,
    zswap: Option<u64>,
    zswapped: Option<u64>,
}

fn read_meminfo() -> MemInfo {
    let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    parse_meminfo(&text)
}

fn parse_meminfo(text: &str) -> MemInfo {
    let kb_opt = |key: &str| -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|v| v.saturating_mul(1024))
    };
    let kb = |key: &str| kb_opt(key).unwrap_or(0);
    let swap_total = kb("SwapTotal:");
    let swap_free = kb("SwapFree:");
    let shmem = kb("Shmem:");
    let cached = kb("Cached:");
    MemInfo {
        total: kb("MemTotal:"),
        // MemAvailable, not MemFree. On any machine with a page cache MemFree is always small, and
        // judging by it reports every healthy machine as short of memory.
        available: kb("MemAvailable:"),
        free: kb("MemFree:"),
        anon: kb("AnonPages:"),
        // Cached includes tmpfs/shmem. The exclusive composition rendered by the UI gives shmem
        // its own segment, so subtract it here rather than stacking the same pages twice.
        file_cache: cached.saturating_sub(shmem),
        shmem,
        buffers: kb("Buffers:"),
        // KReclaimable includes reclaimable slab and newer shrinker-backed direct allocations.
        // Older kernels have only SReclaimable, which is the closest honest fallback.
        kernel_reclaimable: kb_opt("KReclaimable:")
            .or_else(|| kb_opt("SReclaimable:"))
            .unwrap_or(0),
        slab_unreclaimable: kb("SUnreclaim:"),
        unevictable: kb("Unevictable:"),
        mlocked: kb("Mlocked:"),
        dirty: kb("Dirty:"),
        writeback: kb("Writeback:"),
        swap_total,
        swap_used: swap_total.saturating_sub(swap_free),
        swap_cached: kb("SwapCached:"),
        zswap: kb_opt("Zswap:"),
        zswapped: kb_opt("Zswapped:"),
    }
}

fn read_vmstat() -> VmCounters {
    fs::read_to_string("/proc/vmstat")
        .map(|text| parse_vmstat(&text))
        .unwrap_or_default()
}

fn parse_vmstat(text: &str) -> VmCounters {
    let values: BTreeMap<&str, u64> = text
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?, parts.next()?.parse().ok()?))
        })
        .collect();
    let direct_reclaim_pages = values.get("pgscan_direct").copied().unwrap_or_else(|| {
        values
            .iter()
            .filter(|(key, _)| key.starts_with("pgscan_direct_"))
            .map(|(_, value)| *value)
            .sum()
    });
    VmCounters {
        oom_kills: values.get("oom_kill").copied().unwrap_or(0),
        swap_in_pages: values.get("pswpin").copied().unwrap_or(0),
        swap_out_pages: values.get("pswpout").copied().unwrap_or(0),
        major_faults: values.get("pgmajfault").copied().unwrap_or(0),
        direct_reclaim_pages,
        foll_pin_acquired: values.get("nr_foll_pin_acquired").copied(),
        foll_pin_released: values.get("nr_foll_pin_released").copied(),
    }
}

fn read_pressure(path: &str) -> PressureTotals {
    fs::read_to_string(path)
        .map(|text| parse_pressure(&text))
        .unwrap_or_default()
}

fn parse_pressure(text: &str) -> PressureTotals {
    let total = |kind: &str| -> Option<u64> {
        text.lines()
            .find(|line| line.starts_with(kind))?
            .split_whitespace()
            .find_map(|field| field.strip_prefix("total="))?
            .parse()
            .ok()
    };
    PressureTotals {
        some_usec: total("some "),
        full_usec: total("full "),
    }
}

fn read_softirqs() -> Option<SoftirqCounters> {
    let text = fs::read_to_string("/proc/softirqs").ok()?;
    Some(parse_softirqs(&text))
}

fn parse_softirqs(text: &str) -> SoftirqCounters {
    let sum = |kind: &str| -> u64 {
        text.lines()
            .find_map(|line| line.trim_start().strip_prefix(kind))
            .map(|rest| {
                rest.split_whitespace()
                    .filter_map(|value| value.parse::<u64>().ok())
                    .sum()
            })
            .unwrap_or(0)
    };
    SoftirqCounters {
        net_rx: sum("NET_RX:"),
        net_tx: sum("NET_TX:"),
    }
}

fn read_cpu_throttled_usec() -> Option<u64> {
    if let Ok(text) = fs::read_to_string("/sys/fs/cgroup/cpu.stat") {
        return parse_named_counter(&text, "throttled_usec");
    }
    // cgroup v1 reports nanoseconds under a controller-specific directory.
    fs::read_to_string("/sys/fs/cgroup/cpu/cpu.stat")
        .ok()
        .and_then(|text| parse_named_counter(&text, "throttled_time"))
        .map(|nanos| nanos / 1000)
}

fn parse_named_counter(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next()? == key)
            .then(|| fields.next()?.parse().ok())
            .flatten()
    })
}

#[derive(Clone, Copy, Default)]
struct LoadAverage {
    one: f32,
    five: f32,
    fifteen: f32,
    procs_running: Option<u64>,
    procs_total: Option<u64>,
}

fn read_loadavg() -> LoadAverage {
    fs::read_to_string("/proc/loadavg")
        .map(|text| parse_loadavg(&text))
        .unwrap_or_default()
}

fn parse_loadavg(text: &str) -> LoadAverage {
    let mut fields = text.split_whitespace();
    let one = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let five = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let fifteen = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let (procs_running, procs_total) = fields
        .next()
        .and_then(|field| field.split_once('/'))
        .map(|(running, total)| (running.parse().ok(), total.parse().ok()))
        .unwrap_or((None, None));
    LoadAverage {
        one,
        five,
        fifteen,
        procs_running,
        procs_total,
    }
}

fn read_uptime() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| {
            t.split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
        })
        .unwrap_or(0.0) as u64
}

struct DiskInfo {
    total_bytes: u64,
    free_bytes: u64,
    inode_free_pct: f32,
    inode_total: Option<u64>,
    inode_free: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiskIdentity {
    mount: String,
    filesystem: String,
    device: String,
    read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EphemeralPortRange {
    pub(crate) low: u16,
    pub(crate) high: u16,
    pub(crate) capacity: u64,
    pub(crate) reserved: Vec<(u16, u16)>,
}

impl EphemeralPortRange {
    pub(crate) fn contains(&self, port: u16) -> bool {
        (self.low..=self.high).contains(&port)
            && !self
                .reserved
                .iter()
                .any(|(start, end)| (*start..=*end).contains(&port))
    }
}

/// The filesystem holding the state directory, because that is where the spool lives: a full disk
/// fails the spool's fsync and the spool holds accounting. Free space is the leading indicator for
/// that; the spool's own `dropped` counter only says so afterwards.
fn read_disk(state_dir: &Path) -> DiskInfo {
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    let Ok(path) = CString::new(state_dir.as_os_str().as_encoded_bytes()) else {
        return DiskInfo {
            total_bytes: 0,
            free_bytes: 0,
            inode_free_pct: 100.0,
            inode_total: None,
            inode_free: None,
        };
    };
    if unsafe { libc::statvfs(path.as_ptr(), &mut buf) } != 0 {
        return DiskInfo {
            total_bytes: 0,
            free_bytes: 0,
            inode_free_pct: 100.0,
            inode_total: None,
            inode_free: None,
        };
    }
    let block = buf.f_frsize.max(1) as u64;
    DiskInfo {
        total_bytes: buf.f_blocks as u64 * block,
        // f_bavail, not f_bfree: the difference is the root reserve, which we cannot write into
        // and which would make a full disk read as having gigabytes left.
        free_bytes: buf.f_bavail as u64 * block,
        inode_free_pct: if buf.f_files == 0 {
            // Filesystems without a fixed inode table (btrfs, some tmpfs) report zero. Not a
            // shortage — reporting 0% free would light a permanent alarm on them.
            100.0
        } else {
            (buf.f_favail as f64 / buf.f_files as f64 * 100.0) as f32
        },
        inode_total: (buf.f_files > 0).then_some(buf.f_files as u64),
        inode_free: (buf.f_files > 0).then_some(buf.f_favail as u64),
    }
}

fn read_disk_counters(state_dir: &Path) -> Option<DiskCounters> {
    let metadata = fs::metadata(state_dir).ok()?;
    let device = metadata.dev();
    let major = libc::major(device as libc::dev_t) as u64;
    let minor = libc::minor(device as libc::dev_t) as u64;
    let text = fs::read_to_string("/proc/diskstats").ok()?;
    parse_diskstats(&text, major, minor)
}

fn parse_diskstats(text: &str, want_major: u64, want_minor: u64) -> Option<DiskCounters> {
    for line in text.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 14
            || fields[0].parse::<u64>().ok()? != want_major
            || fields[1].parse::<u64>().ok()? != want_minor
        {
            continue;
        }
        let number = |index: usize| fields.get(index)?.parse::<u64>().ok();
        return Some(DiskCounters {
            major: want_major,
            minor: want_minor,
            reads: number(3)?,
            sectors_read: number(5)?,
            read_ms: number(6)?,
            writes: number(7)?,
            sectors_written: number(9)?,
            write_ms: number(10)?,
            in_flight: number(11)?,
            busy_ms: number(12)?,
            weighted_ms: number(13)?,
        });
    }
    None
}

/// Resolve the longest mountpoint containing `state_dir`. mountinfo escapes whitespace and
/// backslashes as octal sequences; decoding those before using `Path::starts_with` avoids both a
/// false prefix match (`/var/lib/a` vs `/var/lib/agent`) and broken labels on escaped paths.
fn read_disk_identity(state_dir: &Path) -> Option<DiskIdentity> {
    let target = fs::canonicalize(state_dir).ok()?;
    let text = fs::read_to_string("/proc/self/mountinfo").ok()?;
    parse_mountinfo(&text, &target)
}

fn parse_mountinfo(text: &str, target: &Path) -> Option<DiskIdentity> {
    let mut best: Option<(usize, DiskIdentity)> = None;
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let left = left.split_whitespace().collect::<Vec<_>>();
        let right = right.split_whitespace().collect::<Vec<_>>();
        if left.len() < 6 || right.len() < 2 {
            continue;
        }
        let mount = decode_mount_field(left[4]);
        let mount_path = Path::new(&mount);
        if !target.starts_with(mount_path) {
            continue;
        }
        let identity = DiskIdentity {
            mount: mount.clone(),
            filesystem: decode_mount_field(right[0]),
            device: decode_mount_field(right[1]),
            read_only: left[5].split(',').any(|option| option == "ro"),
        };
        let specificity = mount_path.as_os_str().len();
        if best
            .as_ref()
            .is_none_or(|(previous, _)| specificity > *previous)
        {
            best = Some((specificity, identity));
        }
    }
    best.map(|(_, identity)| identity)
}

fn decode_mount_field(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &bytes[index + 1..index + 4];
            if octal.iter().all(|byte| matches!(byte, b'0'..=b'7')) {
                out.push((octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + octal[2] - b'0');
                index += 4;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn read_ephemeral_port_range() -> Option<EphemeralPortRange> {
    let text = fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").ok()?;
    let mut values = text.split_whitespace();
    let low = values.next()?.parse::<u16>().ok()?;
    let high = values.next()?.parse::<u16>().ok()?;
    if low > high {
        return None;
    }
    let reserved =
        fs::read_to_string("/proc/sys/net/ipv4/ip_local_reserved_ports").unwrap_or_default();
    Some(ephemeral_port_range(low, high, &reserved))
}

fn ephemeral_port_range(low: u16, high: u16, reserved: &str) -> EphemeralPortRange {
    let mut unavailable = vec![false; usize::from(high - low) + 1];
    let mut ranges = Vec::new();
    for item in reserved.trim().split(',').filter(|item| !item.is_empty()) {
        let (start, end) = item
            .split_once('-')
            .map_or((item, item), |(start, end)| (start, end));
        let (Ok(start), Ok(end)) = (start.parse::<u16>(), end.parse::<u16>()) else {
            continue;
        };
        let start = start.max(low);
        let end = end.min(high);
        if start > end {
            continue;
        }
        ranges.push((start, end));
        for port in start..=end {
            unavailable[usize::from(port - low)] = true;
        }
    }
    EphemeralPortRange {
        low,
        high,
        capacity: unavailable.iter().filter(|reserved| !**reserved).count() as u64,
        reserved: ranges,
    }
}

/// The interface carrying the default route.
///
/// Summing every interface would count wg0 and any tun on top of the physical one, reporting a
/// machine's own traffic two or three times.
pub(crate) fn main_interface() -> Option<String> {
    let route = fs::read_to_string("/proc/net/route").ok()?;
    for line in route.lines().skip(1) {
        let mut f = line.split_whitespace();
        let iface = f.next()?;
        let destination = f.next()?;
        if destination == "00000000" {
            return Some(iface.to_owned());
        }
    }
    None
}

fn read_net() -> NetCounters {
    let Some(nic) = main_interface() else {
        return NetCounters::default();
    };
    let Ok(text) = fs::read_to_string("/proc/net/dev") else {
        return NetCounters::default();
    };
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() != nic {
            continue;
        }
        let f: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        let get = |i: usize| f.get(i).copied().unwrap_or(0);
        // rx: bytes packets errs drop fifo frame compressed multicast
        // tx: bytes packets errs drop fifo colls carrier compressed
        return NetCounters {
            rx_bytes: get(0),
            rx_drop: get(3),
            tx_bytes: get(8),
            tx_drop: get(11),
            errs: get(2) + get(10),
        };
    }
    NetCounters::default()
}

/// Host-wide TCP/UDP counters. This deliberately uses the kernel's aggregate MIB/sockstat views,
/// not `/proc/net/tcp`: walking every socket every ten seconds gets more expensive precisely when
/// the machine is under connection pressure, while the aggregate files remain constant-size.
fn read_network() -> NetworkRaw {
    let snmp = fs::read_to_string("/proc/net/snmp").unwrap_or_default();
    let netstat = fs::read_to_string("/proc/net/netstat").unwrap_or_default();
    let snmp6 = fs::read_to_string("/proc/net/snmp6").unwrap_or_default();
    let sockstat = fs::read_to_string("/proc/net/sockstat").unwrap_or_default();
    let sockstat6 = fs::read_to_string("/proc/net/sockstat6").unwrap_or_default();

    network_from_text(&snmp, &netstat, &snmp6, &sockstat, &sockstat6)
}

fn network_from_text(
    snmp: &str,
    netstat: &str,
    snmp6: &str,
    sockstat: &str,
    sockstat6: &str,
) -> NetworkRaw {
    let tcp = parse_mib_group(snmp, "Tcp");
    let udp = parse_mib_group(snmp, "Udp");
    let tcp_ext = parse_mib_group(netstat, "TcpExt");
    let ipv6 = parse_single_value_mib(snmp6);
    let tcp_sock = parse_sockstat_group(sockstat, "TCP");
    let udp_sock = parse_sockstat_group(sockstat, "UDP");
    let tcp6_sock = parse_sockstat_group(sockstat6, "TCP6");
    let udp6_sock = parse_sockstat_group(sockstat6, "UDP6");

    let get = |table: &BTreeMap<String, u64>, key: &str| table.get(key).copied();
    let add = |left: Option<u64>, right: Option<u64>| match (left, right) {
        (None, None) => None,
        (left, right) => Some(left.unwrap_or(0).saturating_add(right.unwrap_or(0))),
    };
    let udp_both = |v4: &str, v6: &str| add(get(&udp, v4), get(&ipv6, v6));

    NetworkRaw {
        tcp_curr_estab: get(&tcp, "CurrEstab"),
        tcp_inuse: add(get(&tcp_sock, "inuse"), get(&tcp6_sock, "inuse")),
        tcp_time_wait: get(&tcp_sock, "tw"),
        tcp_orphan: get(&tcp_sock, "orphan"),
        tcp_alloc: get(&tcp_sock, "alloc"),
        tcp_mem_pages: get(&tcp_sock, "mem"),
        udp_inuse: add(get(&udp_sock, "inuse"), get(&udp6_sock, "inuse")),
        udp_mem_pages: get(&udp_sock, "mem"),
        tcp_active_opens: get(&tcp, "ActiveOpens"),
        tcp_passive_opens: get(&tcp, "PassiveOpens"),
        tcp_attempt_fails: get(&tcp, "AttemptFails"),
        tcp_estab_resets: get(&tcp, "EstabResets"),
        tcp_retrans_segs: get(&tcp, "RetransSegs"),
        tcp_syn_retrans: get(&tcp_ext, "TCPSynRetrans"),
        tcp_in_errors: get(&tcp, "InErrs"),
        tcp_out_resets: get(&tcp, "OutRsts"),
        tcp_timeouts: get(&tcp_ext, "TCPTimeouts"),
        tcp_listen_overflows: get(&tcp_ext, "ListenOverflows"),
        tcp_listen_drops: get(&tcp_ext, "ListenDrops"),
        udp_in_errors: udp_both("InErrors", "Udp6InErrors"),
        udp_no_ports: udp_both("NoPorts", "Udp6NoPorts"),
        udp_rcvbuf_errors: udp_both("RcvbufErrors", "Udp6RcvbufErrors"),
        udp_sndbuf_errors: udp_both("SndbufErrors", "Udp6SndbufErrors"),
    }
}

/// `/proc/net/snmp` and `/proc/net/netstat` encode one group as two adjacent rows: field names,
/// then values. Zip by name rather than pinning column offsets; kernels append counters over time.
fn parse_mib_group(text: &str, group: &str) -> BTreeMap<String, u64> {
    let prefix = format!("{group}:");
    let mut lines = text.lines();
    while let Some(header) = lines.next() {
        if !header.starts_with(&prefix) {
            continue;
        }
        let Some(values) = lines.next() else {
            break;
        };
        if !values.starts_with(&prefix) {
            continue;
        }
        return header
            .split_whitespace()
            .skip(1)
            .zip(values.split_whitespace().skip(1))
            .filter_map(|(name, value)| value.parse().ok().map(|value| (name.to_owned(), value)))
            .collect();
    }
    BTreeMap::new()
}

/// `/proc/net/snmp6` is one `name value` pair per row (unlike the IPv4 table above).
fn parse_single_value_mib(text: &str) -> BTreeMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let value = fields.next()?.parse().ok()?;
            Some((name.to_owned(), value))
        })
        .collect()
}

fn parse_sockstat_group(text: &str, group: &str) -> BTreeMap<String, u64> {
    let prefix = format!("{group}:");
    let Some(line) = text.lines().find(|line| line.starts_with(&prefix)) else {
        return BTreeMap::new();
    };
    let fields: Vec<&str> = line.split_whitespace().skip(1).collect();
    fields
        .chunks_exact(2)
        .filter_map(|pair| {
            pair[1]
                .parse()
                .ok()
                .map(|value| (pair[0].to_owned(), value))
        })
        .collect()
}

/// The processes we track, by the `comm` the kernel reports for each.
const WATCHED: [&str; 5] = [
    "xray",
    "phantun-client",
    "brocade-agent",
    "wireguard-go",
    "boringtun",
];

/// Find all five pids in **one** walk of /proc.
///
/// The obvious shape — a `find_pid(name)` called once per process — walks /proc five times per
/// sample, and this samples every 10 seconds. On a machine with 200 processes that is 1 000 `comm`
/// reads every 10 seconds to find five numbers, and the process table used to walk it a second
/// time on top of that. Spent by the very thread whose job is to report that this machine is not
/// overloaded.
///
/// **Zombies are skipped, and that is not a theoretical nicety.** A preview node had two entries
/// with `comm == "xray"`: the live one, and a zombie left behind by an earlier restart. A zombie
/// keeps its `comm`, its `stat` and its `limits`, but its memory is already reclaimed and its fd
/// directory is empty — so picking it reported xray as using no memory and holding no file
/// descriptors, on a machine where xray was serving traffic perfectly well. The agent `nohup`s
/// xray, so a zombie surviving a restart is ordinary rather than exceptional.
///
/// Among the survivors, the **earliest started** wins. e2e probing launches a short-lived xray per
/// chain (see e2e.rs), so at any moment there may be a second, legitimate, live xray that is not
/// the one serving traffic. The service process started first; a prober started seconds ago.
fn find_pids() -> BTreeMap<String, u32> {
    // name -> (starttime in clock ticks, pid). Smallest starttime wins.
    let mut best: BTreeMap<String, (u64, u32)> = BTreeMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return BTreeMap::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(found) = fs::read_to_string(format!("/proc/{pid}/comm")) else {
            continue;
        };
        let found = found.trim();
        let Some(watched) = WATCHED.iter().find(|w| **w == found) else {
            continue;
        };
        let Some(fields) = stat_fields(pid) else {
            continue;
        };
        if fields.first().map(String::as_str) == Some("Z") {
            continue;
        }
        let Some(started) = fields.get(19).and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        best.entry((*watched).to_owned())
            .and_modify(|slot| {
                if started < slot.0 {
                    *slot = (started, pid);
                }
            })
            .or_insert((started, pid));
    }
    best.into_iter()
        .map(|(name, (_, pid))| (name, pid))
        .collect()
}

/// utime+stime per watched process, in jiffies, from the pids the walk already found.
fn read_proc_cpu(pids: &BTreeMap<String, u32>) -> BTreeMap<String, u64> {
    pids.iter()
        .filter_map(|(name, pid)| read_proc_jiffies(*pid).map(|j| (name.clone(), j)))
        .collect()
}

/// `/proc/<pid>/stat` is space-separated except for field 2, `comm`, which is parenthesised and may
/// itself contain spaces and parentheses. Splitting the whole line on whitespace therefore
/// mis-indexes every field after it — the classic way to read this file wrong. Cutting at the last
/// `)` is the documented way round it.
fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tail = &text[text.rfind(')')? + 1..];
    Some(tail.split_whitespace().map(str::to_owned).collect())
}

fn read_proc_jiffies(pid: u32) -> Option<u64> {
    let f = stat_fields(pid)?;
    // After the `)` cut, index 0 is `state` (field 3), so utime (field 14) is at 11 and stime at 12.
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    Some(utime + stime)
}

fn read_started_at(pid: u32, btime: i64) -> Option<i64> {
    let f = stat_fields(pid)?;
    // starttime is field 22, so index 19 after the cut. It counts clock ticks since boot.
    let ticks: u64 = f.get(19)?.parse().ok()?;
    let hz = clock_ticks();
    Some(btime + (ticks / hz) as i64)
}

fn clock_ticks() -> u64 {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 {
        hz as u64
    } else {
        100
    }
}

fn num_cores() -> u32 {
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n > 0 {
        n as u32
    } else {
        1
    }
}

fn page_size() -> u64 {
    let bytes = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if bytes > 0 {
        bytes as u64
    } else {
        4096
    }
}

/// Mean cpufreq value across the CPUs that expose it. sysfs reports kHz.
fn read_cpu_frequency_mhz(file: &str) -> Option<u64> {
    let values: Vec<u64> = (0..num_cores())
        .filter_map(|cpu| {
            read_u64_file(&format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/{file}"))
        })
        .collect();
    (!values.is_empty()).then(|| values.iter().sum::<u64>() / values.len() as u64 / 1000)
}

fn read_cpu_max_frequency_mhz() -> Option<u64> {
    (0..num_cores())
        .filter_map(|cpu| {
            read_u64_file(&format!(
                "/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_max_freq"
            ))
        })
        .max()
        .map(|khz| khz / 1000)
}

fn read_cpu_governor() -> Option<String> {
    let governors: Vec<String> = (0..num_cores())
        .filter_map(|cpu| {
            read_trimmed(&format!(
                "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor"
            ))
        })
        .collect();
    let first = governors.first()?.clone();
    governors
        .iter()
        .all(|value| value == &first)
        .then_some(first)
}

fn read_rss(pid: u32) -> Option<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb: u64 = text
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

fn count_fds(pid: u32) -> Option<u64> {
    Some(fs::read_dir(format!("/proc/{pid}/fd")).ok()?.count() as u64)
}

fn read_fd_limit(pid: u32) -> Option<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/limits")).ok()?;
    let line = text.lines().find(|l| l.starts_with("Max open files"))?;
    // `Max open files            1024                 4096                 files`
    line.split_whitespace().nth(3).and_then(|v| v.parse().ok())
}

fn read_trimmed(path: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `None` rather than 0 when the file is absent. For conntrack that distinction is the whole
/// point: the module not being loaded is a machine doing no NAT, while an empty table on a busy
/// NAT node is a real and different alarm.
fn read_u64_file(path: &str) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  1000 100 500 8000 50 20 80 0 0 0\n\
                        cpu0 500 50 250 4000 25 10 40 0 0 0\n\
                        btime 1700000000\n\
                        processes 12345\n";

    #[test]
    fn folds_nice_into_user_and_irq_into_system_but_keeps_softirq_apart() {
        let cpu = parse_cpu(STAT);
        assert_eq!(cpu.user, 1100, "nice belongs to user");
        assert_eq!(cpu.system, 520, "irq belongs to system");
        assert_eq!(
            cpu.softirq, 80,
            "softirq stays on its own — it is the whole point of the split"
        );
        assert_eq!(cpu.total, 9750);
        assert_eq!(cpu.iowait, 50, "iowait stays outside busy CPU");
        let cores = parse_cpu_cores(STAT);
        assert_eq!(cores.len(), 1);
        assert_eq!(cores[&0].user, 550);
    }

    #[test]
    fn meminfo_composition_does_not_count_shmem_twice() {
        let mem = parse_meminfo(
            "MemTotal: 8192 kB\nMemFree: 1024 kB\nMemAvailable: 3072 kB\n\
             Cached: 2048 kB\nShmem: 256 kB\nBuffers: 64 kB\nAnonPages: 3072 kB\n\
             KReclaimable: 512 kB\nSReclaimable: 400 kB\nSUnreclaim: 128 kB\n\
             Unevictable: 32 kB\nMlocked: 16 kB\nDirty: 8 kB\nWriteback: 4 kB\n\
             SwapTotal: 4096 kB\nSwapFree: 3072 kB\nSwapCached: 12 kB\n\
             Zswap: 20 kB\nZswapped: 80 kB\n",
        );
        assert_eq!(mem.file_cache, (2048 - 256) * 1024);
        assert_eq!(mem.shmem, 256 * 1024);
        assert_eq!(
            mem.kernel_reclaimable,
            512 * 1024,
            "KReclaimable wins when present"
        );
        assert_eq!(mem.swap_used, 1024 * 1024);
        assert_eq!(mem.zswap, Some(20 * 1024));
    }

    #[test]
    fn pressure_uses_cumulative_totals_not_rounded_averages() {
        let pressure = parse_pressure(
            "some avg10=0.21 avg60=0.04 avg300=0.01 total=123456\n\
             full avg10=0.00 avg60=0.00 avg300=0.00 total=789\n",
        );
        assert_eq!(pressure.some_usec, Some(123_456));
        assert_eq!(pressure.full_usec, Some(789));
    }

    #[test]
    fn vmstat_keeps_swap_fault_reclaim_and_pin_counters_distinct() {
        let vm = parse_vmstat(
            "oom_kill 2\npswpin 11\npswpout 17\npgmajfault 23\n\
             pgscan_direct_dma 3\npgscan_direct_normal 5\n\
             nr_foll_pin_acquired 100\nnr_foll_pin_released 96\n",
        );
        assert_eq!(vm.oom_kills, 2);
        assert_eq!(vm.swap_in_pages, 11);
        assert_eq!(vm.swap_out_pages, 17);
        assert_eq!(vm.major_faults, 23);
        assert_eq!(vm.direct_reclaim_pages, 8);
        assert_eq!(vm.foll_pin_acquired, Some(100));
        assert_eq!(vm.foll_pin_released, Some(96));
    }

    #[test]
    fn loadavg_carries_all_horizons_and_the_run_queue() {
        let load = parse_loadavg("0.42 0.38 0.31 2/184 1234\n");
        assert_eq!(load.one, 0.42);
        assert_eq!(load.five, 0.38);
        assert_eq!(load.fifteen, 0.31);
        assert_eq!(load.procs_running, Some(2));
        assert_eq!(load.procs_total, Some(184));
    }

    #[test]
    fn network_tables_are_joined_by_name_and_include_ipv6_socket_and_udp_counts() {
        let network = network_from_text(
            "Tcp: ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InErrs OutRsts RetransSegs\n\
             Tcp: 100 200 3 4 55 6 7 8\n\
             Udp: NoPorts InErrors RcvbufErrors SndbufErrors\n\
             Udp: 9 10 11 12\n",
            "TcpExt: ListenOverflows ListenDrops TCPTimeouts TCPSynRetrans\n\
             TcpExt: 13 14 15 16\n",
            "Udp6NoPorts 1\nUdp6InErrors 2\nUdp6RcvbufErrors 3\nUdp6SndbufErrors 4\n",
            "TCP: inuse 20 orphan 2 tw 30 alloc 40 mem 50\nUDP: inuse 6 mem 7\n",
            "TCP6: inuse 8\nUDP6: inuse 9\n",
        );

        assert_eq!(network.tcp_curr_estab, Some(55));
        assert_eq!(network.tcp_inuse, Some(28));
        assert_eq!(network.tcp_time_wait, Some(30));
        assert_eq!(network.tcp_mem_pages, Some(50));
        assert_eq!(network.udp_inuse, Some(15));
        assert_eq!(network.tcp_retrans_segs, Some(8));
        assert_eq!(network.tcp_listen_overflows, Some(13));
        assert_eq!(network.tcp_syn_retrans, Some(16));
        assert_eq!(network.udp_no_ports, Some(10));
        assert_eq!(network.udp_in_errors, Some(12));
        assert_eq!(network.udp_rcvbuf_errors, Some(14));
        assert_eq!(network.udp_sndbuf_errors, Some(16));
    }

    #[test]
    fn network_window_keeps_the_last_inventory_and_sums_events() {
        let network = |level: u64, events: u64| NetworkDetailSample {
            tcp_curr_estab: Some(level),
            tcp_inuse: Some(level + 1),
            tcp_time_wait: Some(level + 2),
            tcp_orphan: Some(level + 3),
            tcp_alloc: Some(level + 4),
            tcp_mem_bytes: Some(level + 5),
            udp_inuse: Some(level + 6),
            udp_mem_bytes: Some(level + 7),
            tcp_active_opens: Some(events),
            tcp_passive_opens: Some(events),
            tcp_attempt_fails: Some(events),
            tcp_estab_resets: Some(events),
            tcp_retrans_segs: Some(events),
            tcp_syn_retrans: Some(events),
            tcp_in_errors: Some(events),
            tcp_out_resets: Some(events),
            tcp_timeouts: Some(events),
            tcp_listen_overflows: Some(events),
            tcp_listen_drops: Some(events),
            udp_in_errors: Some(events),
            udp_no_ports: Some(events),
            udp_rcvbuf_errors: Some(events),
            udp_sndbuf_errors: Some(events),
            ..Default::default()
        };
        let sub = |start: u64, level: u64, events: u64| Sub {
            window_start_unix_secs: start,
            window_end_unix_secs: start + 10,
            elapsed_secs: 10,
            network_detail: Some(network(level, events)),
            ..Default::default()
        };
        let sample = aggregate(
            &[sub(100, 10, 1), sub(110, 20, 2), sub(120, 30, 3)],
            Path::new("/tmp"),
        );
        let detail = sample.network_detail.expect("network detail");
        assert_eq!(
            detail.tcp_curr_estab,
            Some(30),
            "inventory is the final level"
        );
        assert_eq!(detail.udp_inuse, Some(36));
        assert_eq!(
            detail.tcp_active_opens,
            Some(6),
            "events add over the window"
        );
        assert_eq!(detail.tcp_retrans_segs, Some(6));
        assert_eq!(detail.udp_rcvbuf_errors, Some(6));
    }

    #[test]
    fn diskstats_are_attributed_by_device_number_not_by_a_name_guess() {
        let text = "   8       0 sda 10 1 200 30 40 2 600 90 3 120 240 0 0 0 0\n\
                    8       1 sda1 7 0 100 20 30 0 400 80 2 100 180 0 0 0 0\n";
        let disk = parse_diskstats(text, 8, 1).unwrap();
        assert_eq!(disk.reads, 7);
        assert_eq!(disk.sectors_read, 100);
        assert_eq!(disk.writes, 30);
        assert_eq!(disk.sectors_written, 400);
        assert_eq!(disk.in_flight, 2);
        assert_eq!(disk.weighted_ms, 180);
        assert!(parse_diskstats(text, 253, 0).is_none());
    }

    #[test]
    fn disk_window_preserves_rates_latency_busy_queue_and_pressure() {
        let raw = |at: u64, reads: u64, writes: u64, read_ms: u64, write_ms: u64| Raw {
            at,
            btime: 1,
            cpu: CpuTimes {
                total: at * 10,
                ..Default::default()
            },
            disk: Some(DiskCounters {
                major: 8,
                minor: 1,
                reads,
                sectors_read: reads * 8,
                read_ms,
                writes,
                sectors_written: writes * 16,
                write_ms,
                in_flight: 2,
                busy_ms: at * 100,
                weighted_ms: at * 200,
            }),
            io_pressure: PressureTotals {
                some_usec: Some(at * 10_000),
                full_usec: Some(at * 1_000),
            },
            ..Default::default()
        };
        let one = difference(&raw(100, 10, 20, 40, 100), &raw(110, 30, 30, 80, 140));
        let two = difference(&raw(110, 30, 30, 80, 140), &raw(120, 50, 40, 120, 180));
        let three = difference(&raw(120, 50, 40, 120, 180), &raw(130, 70, 50, 160, 220));
        let sample = aggregate(&[one, two, three], Path::new("/tmp"));
        let detail = sample.disk_detail.unwrap();
        assert_eq!(detail.read_bps, Some(8_192));
        assert_eq!(detail.write_bps, Some(8_192));
        assert_eq!(detail.read_iops, Some(2.0));
        assert_eq!(detail.write_iops, Some(1.0));
        assert_eq!(detail.read_await_ms, Some(2.0));
        assert_eq!(detail.write_await_ms, Some(4.0));
        assert_eq!(detail.busy_pct, Some(10.0));
        assert_eq!(detail.queue_depth, Some(0.2));
        assert_eq!(detail.in_flight, Some(2));
        assert_eq!(detail.pressure_some_pct, Some(1.0));
        assert_eq!(detail.pressure_full_pct, Some(0.1));
    }

    #[test]
    fn mountinfo_uses_the_most_specific_mount_and_decodes_fields() {
        let text = "20 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw\n\
                    21 20 8:2 / /var/lib/brocade\\040agent ro,relatime - xfs /dev/vdb1 ro\n";
        let identity = parse_mountinfo(text, Path::new("/var/lib/brocade agent/state")).unwrap();
        assert_eq!(identity.mount, "/var/lib/brocade agent");
        assert_eq!(identity.filesystem, "xfs");
        assert_eq!(identity.device, "/dev/vdb1");
        assert!(identity.read_only);
    }

    #[test]
    fn reserved_ephemeral_ranges_are_clipped_deduplicated_and_subtracted() {
        let range = ephemeral_port_range(100, 109, "90-101,103,103-105,200");
        assert_eq!(range.capacity, 5); // reserved: 100, 101, 103, 104, 105
        assert!(!range.contains(100));
        assert!(range.contains(102));
        assert!(!range.contains(200));
    }

    #[test]
    fn reads_btime() {
        assert_eq!(parse_btime(STAT), Some(1_700_000_000));
    }

    #[test]
    fn cpu_model_prefers_the_human_readable_x86_name_and_normalizes_spacing() {
        let cpuinfo = "processor : 0\nmodel name : Intel(R)   Xeon(R) Platinum 8370C\n";
        assert_eq!(parse_cpu_model(cpuinfo), "Intel(R) Xeon(R) Platinum 8370C");
    }

    #[test]
    fn arm_cpu_part_becomes_a_model_when_cpuinfo_has_no_model_name() {
        let cpuinfo = "processor\t: 0\nCPU implementer\t: 0x41\nCPU part\t: 0xd0c\n";
        assert_eq!(parse_cpu_model(cpuinfo), "Neoverse-N1");
    }

    #[test]
    fn an_unknown_arm_cpu_still_has_a_stable_identity() {
        let cpuinfo = "CPU implementer : 0xab\nCPU part : 0x123\n";
        assert_eq!(parse_cpu_model(cpuinfo), "ARM 0xab:0x123");
    }

    /// A reboot resets every counter. Reported as growth it would say a machine that just came up
    /// moved its whole lifetime's bytes in ten seconds.
    #[test]
    fn a_reboot_zeroes_the_deltas_and_flags_the_gap() {
        let before = Raw {
            at: 100,
            btime: 1_700_000_000,
            cpu: CpuTimes {
                user: 1000,
                system: 500,
                softirq: 80,
                steal: 0,
                total: 9750,
                ..Default::default()
            },
            net: NetCounters {
                rx_bytes: 1_000_000,
                tx_bytes: 500_000,
                ..Default::default()
            },
            oom_kills: 3,
            proc_cpu: BTreeMap::new(),
            pids: BTreeMap::new(),
            ..Default::default()
        };
        let after = Raw {
            at: 110,
            btime: 1_700_000_500, // rebooted
            cpu: CpuTimes {
                user: 10,
                system: 5,
                softirq: 1,
                steal: 0,
                total: 100,
                ..Default::default()
            },
            net: NetCounters {
                rx_bytes: 200,
                tx_bytes: 100,
                ..Default::default()
            },
            oom_kills: 0,
            proc_cpu: BTreeMap::new(),
            pids: BTreeMap::new(),
            ..Default::default()
        };
        let sub = difference(&before, &after);
        assert!(sub.gap);
        assert_eq!(sub.cpu_user_pct, 0.0);
        assert_eq!(
            sub.nic_rx_bps, 0,
            "not 200 bytes' worth of growth out of nowhere"
        );
        assert_eq!(sub.oom_kills, 0);
    }

    /// Counters going backwards without a reboot happens (CPU hotplug, and per-class jiffies are
    /// not strictly monotonic). A negative percentage would fail the control plane's CHECK and
    /// cost the entire round, not just this field.
    #[test]
    fn counters_going_backwards_clamp_to_zero_rather_than_going_negative() {
        let before = Raw {
            at: 100,
            btime: 1_700_000_000,
            cpu: CpuTimes {
                user: 1000,
                system: 500,
                softirq: 80,
                steal: 0,
                total: 9750,
                ..Default::default()
            },
            net: NetCounters {
                rx_bytes: 1_000_000,
                ..Default::default()
            },
            oom_kills: 0,
            proc_cpu: BTreeMap::new(),
            pids: BTreeMap::new(),
            ..Default::default()
        };
        let after = Raw {
            at: 110,
            btime: 1_700_000_000,
            cpu: CpuTimes {
                user: 990,
                system: 500,
                softirq: 80,
                steal: 0,
                total: 9800,
                ..Default::default()
            },
            net: NetCounters {
                rx_bytes: 900_000,
                ..Default::default()
            },
            oom_kills: 0,
            proc_cpu: BTreeMap::new(),
            pids: BTreeMap::new(),
            ..Default::default()
        };
        let sub = difference(&before, &after);
        assert!(
            sub.gap,
            "a CPU counter regression makes this interval incomparable even without a reboot"
        );
        assert_eq!(sub.cpu_user_pct, 0.0);
        assert_eq!(sub.nic_rx_bps, 0);
    }

    #[test]
    fn per_core_iowait_regression_drops_that_core_instead_of_emitting_over_100_percent() {
        let before = Raw {
            at: 100,
            btime: 1_700_000_000,
            cpu: CpuTimes {
                user: 100,
                system: 20,
                softirq: 10,
                iowait: 50,
                total: 1000,
                ..Default::default()
            },
            cpu_cores: BTreeMap::from([(
                0,
                CpuTimes {
                    user: 100,
                    system: 20,
                    softirq: 10,
                    iowait: 50,
                    total: 1000,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let after = Raw {
            at: 110,
            btime: 1_700_000_000,
            cpu: CpuTimes {
                user: 120,
                system: 25,
                softirq: 12,
                iowait: 55,
                total: 1100,
                ..Default::default()
            },
            cpu_cores: BTreeMap::from([(
                0,
                CpuTimes {
                    user: 120,
                    system: 25,
                    softirq: 12,
                    // Linux documents that this counter can move backwards.
                    iowait: 49,
                    total: 1100,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };

        let sub = difference(&before, &after);
        assert!(sub.gap, "counter regression has to break the chart");
        assert!(
            sub.cpu_cores.is_empty(),
            "an invalid core is omitted instead of costing the whole report"
        );
        assert!(sub.cpu_user_pct > 0.0, "the valid aggregate remains usable");
    }

    #[test]
    fn pressure_and_rates_use_the_real_elapsed_interval() {
        let before = Raw {
            at: 100,
            btime: 1,
            cpu: CpuTimes {
                total: 1000,
                ..Default::default()
            },
            io_pressure: PressureTotals {
                some_usec: Some(1_000_000),
                full_usec: Some(500_000),
            },
            net: NetCounters {
                rx_bytes: 100,
                ..Default::default()
            },
            ..Default::default()
        };
        let after = Raw {
            at: 140,
            btime: 1,
            cpu: CpuTimes {
                user: 100,
                total: 1400,
                ..Default::default()
            },
            io_pressure: PressureTotals {
                some_usec: Some(3_000_000),
                full_usec: Some(900_000),
            },
            net: NetCounters {
                rx_bytes: 500,
                ..Default::default()
            },
            ..Default::default()
        };

        let sub = difference(&before, &after);
        assert_eq!(sub.elapsed_secs, 40);
        assert!(
            sub.gap,
            "a 40-second sub-sample is not a normal 10-second point"
        );
        assert_eq!(sub.nic_rx_bps, 80, "400 bytes over 40 seconds");
        assert_eq!(sub.io_pressure_some_pct, Some(5.0));
        assert_eq!(sub.io_pressure_full_pct, Some(1.0));
    }

    #[test]
    fn aggregate_uses_true_boundaries_and_elapsed_weighting() {
        let sub = |start: u64, elapsed: u64, user: f32| Sub {
            window_start_unix_secs: start,
            window_end_unix_secs: start + elapsed,
            elapsed_secs: elapsed,
            cpu_user_pct: user,
            nic_rx_bps: user as u64,
            gap: elapsed > 15,
            ..Default::default()
        };
        let subs = vec![sub(100, 40, 80.0), sub(140, 10, 10.0), sub(150, 10, 10.0)];
        let sample = aggregate(&subs, Path::new("/tmp"));

        assert_eq!(sample.window_start_unix_secs, 100);
        assert_eq!(sample.window_end_unix_secs, 160);
        assert!(sample.has_gap);
        assert!((sample.cpu_user_pct - 56.666_668).abs() < 0.001);
        assert_eq!(sample.nic_rx_bps, 56);
    }

    /// The peak must be over the per-sub-sample sum, not the sum of per-class peaks: three maxima
    /// that never happened together can add past 100 on a machine that was never saturated.
    #[test]
    fn peak_is_the_worst_moment_not_the_sum_of_worst_classes() {
        let sub = |start: u64, u: f32, s: f32, i: f32| Sub {
            window_start_unix_secs: start,
            window_end_unix_secs: start + 10,
            elapsed_secs: 10,
            cpu_user_pct: u,
            cpu_sys_pct: s,
            cpu_softirq_pct: i,
            cpu_steal_pct: 0.0,
            nic_rx_bps: 0,
            nic_tx_bps: 0,
            nic_rx_drop: 0,
            nic_tx_drop: 0,
            nic_err: 0,
            oom_kills: 0,
            proc_cpu_pct: BTreeMap::new(),
            gap: false,
            ..Default::default()
        };
        let subs = vec![
            sub(970, 60.0, 5.0, 5.0),
            sub(980, 5.0, 60.0, 5.0),
            sub(990, 5.0, 5.0, 60.0),
        ];
        let sample = aggregate(&subs, Path::new("/tmp"));
        // Each class peaks at 60; summing those gives 180. The real worst moment is 70.
        assert!(sample.cpu_peak_pct <= 100.0);
        assert!(
            (sample.cpu_peak_pct - 70.0).abs() < 0.01,
            "got {}",
            sample.cpu_peak_pct
        );
        assert_eq!(sample.window_start_unix_secs, 970);
        assert_eq!(sample.window_end_unix_secs, 1000);
    }

    /// The parenthesised `comm` field is the classic way to read /proc/<pid>/stat wrong: a process
    /// named `(evil name)` shifts every later field if the line is simply split on whitespace.
    #[test]
    fn stat_parsing_survives_a_comm_containing_spaces_and_parens() {
        let line = "42 (my (weird) proc) S 1 42 42 0 -1 4194304 100 0 0 0 11 22 0 0 20 0 3 0 999";
        let tail = &line[line.rfind(')').unwrap() + 1..];
        let f: Vec<&str> = tail.split_whitespace().collect();
        assert_eq!(f[0], "S", "index 0 after the cut is state");
        assert_eq!(f[11], "11", "utime");
        assert_eq!(f[12], "22", "stime");
        assert_eq!(f[19], "999", "starttime");
    }
}

#[cfg(test)]
mod pid_tests {
    /// A zombie keeps its comm, its stat and its limits — everything `find_pids` looks at to
    /// decide — while its memory is already reclaimed and its fd directory is empty. Picking one
    /// reports the process as using no memory and holding no descriptors, on a machine where it is
    /// serving traffic fine. Found on a preview node, where xray had a live pid and a zombie left
    /// by an earlier restart.
    #[test]
    fn a_zombie_is_recognised_by_the_state_field() {
        let zombie = "481 (xray) Z 1 481 481 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 29695008";
        let live =
            "544579 (xray) S 1 544579 481 0 -1 4194304 900 0 0 0 31 12 0 0 20 0 14 0 44273868";
        let parse = |line: &str| -> (String, u64) {
            let tail = &line[line.rfind(')').unwrap() + 1..];
            let f: Vec<&str> = tail.split_whitespace().collect();
            (f[0].to_owned(), f[19].parse().unwrap())
        };
        let (zombie_state, zombie_start) = parse(zombie);
        let (live_state, live_start) = parse(live);
        assert_eq!(zombie_state, "Z");
        assert_eq!(live_state, "S");
        // The trap: the zombie started *earlier*, so "earliest wins" on its own picks exactly the
        // wrong one. Skipping zombies has to happen before the starttime comparison, not after.
        assert!(zombie_start < live_start);
    }
}
