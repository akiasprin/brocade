//! Host resource sampling, read straight from `/proc` and `statvfs`.
//!
//! No `sysinfo` crate. It reads all of `/proc` to answer any question, while this needs eight
//! files, and it would take the agent's dependency tree from five crates to dozens — which then
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

use std::{collections::BTreeMap, ffi::CString, fs, path::Path, sync::Mutex};

use brocade_deployment::protocol::{HostFacts, LoadSample, ProcessSample};

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
#[derive(Clone)]
struct Raw {
    at: u64,
    btime: i64,
    /// Total jiffies per class, from `/proc/stat`'s first line.
    cpu: CpuTimes,
    /// Sum across the main interface only, not every interface: lo would double every byte, and a
    /// wg0 or tun would count the same traffic again one layer up.
    net: NetCounters,
    oom_kills: u64,
    /// utime+stime jiffies per process, keyed by the name we asked for.
    proc_cpu: BTreeMap<String, u64>,
    /// The pids behind `proc_cpu`, carried so the process table does not have to walk /proc again.
    pids: BTreeMap<String, u32>,
}

/// One differenced sub-sample: rates over `SUB_INTERVAL_SECS`.
#[derive(Clone)]
struct Sub {
    cpu_user_pct: f32,
    cpu_sys_pct: f32,
    cpu_softirq_pct: f32,
    cpu_steal_pct: f32,
    nic_rx_bps: u64,
    nic_tx_bps: u64,
    nic_rx_drop: u64,
    nic_tx_drop: u64,
    nic_err: u64,
    oom_kills: u64,
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
    /// Tracked separately from the three reported classes: steal is not work this machine does,
    /// it is the hypervisor serving another guest. See `LoadSample::cpu_steal_pct`.
    steal: u64,
    total: u64,
}

#[derive(Clone, Copy, Default)]
struct NetCounters {
    rx_bytes: u64,
    tx_bytes: u64,
    rx_drop: u64,
    tx_drop: u64,
    errs: u64,
}

/// Take one sub-sample. Called every `SUB_INTERVAL_SECS`; returns a finished window every
/// `SUBS_PER_WINDOW` calls and `None` in between.
pub(crate) fn tick(state_dir: &Path, now: u64) -> Option<(LoadSample, Vec<ProcessSample>)> {
    let raw = read_raw();
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

    let window_secs = SUB_INTERVAL_SECS * subs.len() as u64;
    Some((
        aggregate(&subs, state_dir, now, window_secs),
        processes(&subs, &raw),
    ))
}

/// The btime of the reading the caller is about to send. Read separately because the report
/// carries it once rather than per sample.
pub(crate) fn current_btime() -> i64 {
    read_btime().unwrap_or(0)
}

fn difference(before: &Raw, now: &Raw) -> Sub {
    let elapsed = now.at.saturating_sub(before.at).max(1);
    // A reboot resets every counter in here. Treating that as growth would report a machine that
    // just came up as having transferred its whole lifetime's bytes in ten seconds.
    let rebooted = before.btime != now.btime;
    let cpu_total = now.cpu.total.saturating_sub(before.cpu.total);
    let share = |a: u64, b: u64| -> f32 {
        if rebooted || cpu_total == 0 {
            return 0.0;
        }
        // saturating_sub, not a checked subtraction: /proc/stat's per-class counters can go
        // backwards by a jiffy or two across a CPU hotplug, and a negative percentage would fail
        // the control plane's CHECK and cost the whole round.
        (a.saturating_sub(b) as f32 / cpu_total as f32) * 100.0
    };
    let delta = |a: u64, b: u64| -> u64 {
        if rebooted {
            0
        } else {
            a.saturating_sub(b)
        }
    };

    let mut proc_cpu_pct = BTreeMap::new();
    for (name, jiffies) in &now.proc_cpu {
        let before_j = before.proc_cpu.get(name).copied().unwrap_or(0);
        // A process that restarted inside the window has its own counter reset; its jiffies going
        // backwards is that, not negative CPU.
        if rebooted || *jiffies < before_j || cpu_total == 0 {
            proc_cpu_pct.insert(name.clone(), 0.0);
            continue;
        }
        proc_cpu_pct.insert(
            name.clone(),
            ((jiffies - before_j) as f32 / cpu_total as f32) * 100.0,
        );
    }

    Sub {
        cpu_user_pct: share(now.cpu.user, before.cpu.user),
        cpu_sys_pct: share(now.cpu.system, before.cpu.system),
        cpu_softirq_pct: share(now.cpu.softirq, before.cpu.softirq),
        cpu_steal_pct: share(now.cpu.steal, before.cpu.steal),
        nic_rx_bps: delta(now.net.rx_bytes, before.net.rx_bytes) * 8 / elapsed,
        nic_tx_bps: delta(now.net.tx_bytes, before.net.tx_bytes) * 8 / elapsed,
        nic_rx_drop: delta(now.net.rx_drop, before.net.rx_drop),
        nic_tx_drop: delta(now.net.tx_drop, before.net.tx_drop),
        nic_err: delta(now.net.errs, before.net.errs),
        oom_kills: delta(now.oom_kills, before.oom_kills),
        proc_cpu_pct,
        gap: rebooted,
    }
}

/// btime is not on the sample: it is reported once per round, on `LoadReportRequest`. A window
/// cannot disagree with its own report about when the machine booted.
fn aggregate(subs: &[Sub], state_dir: &Path, now: u64, window_secs: u64) -> LoadSample {
    let n = subs.len() as f32;
    let mean = |f: fn(&Sub) -> f32| subs.iter().map(f).sum::<f32>() / n;
    let user = mean(|s| s.cpu_user_pct);
    let sys = mean(|s| s.cpu_sys_pct);
    let softirq = mean(|s| s.cpu_softirq_pct);
    // The peak is over the *sum* per sub-sample, not the sum of per-class peaks: the latter adds
    // three maxima that never occurred together and can exceed 100 on a machine that was never
    // saturated.
    let peak = subs
        .iter()
        .map(|s| s.cpu_user_pct + s.cpu_sys_pct + s.cpu_softirq_pct)
        .fold(0.0_f32, f32::max);

    let mem = read_meminfo();
    let disk = read_disk(state_dir);
    LoadSample {
        window_start_unix_secs: (now.saturating_sub(window_secs)) as i64,
        window_end_unix_secs: now as i64,
        has_gap: subs.iter().any(|s| s.gap),
        cpu_user_pct: user,
        cpu_sys_pct: sys,
        cpu_softirq_pct: softirq,
        cpu_peak_pct: peak.max(user + sys + softirq).min(100.0),
        cpu_steal_pct: mean(|s| s.cpu_steal_pct),
        load1: read_loadavg(),
        mem_available_bytes: mem.available,
        swap_used_bytes: mem.swap_used,
        oom_kills: subs.iter().map(|s| s.oom_kills).sum(),
        disk_free_bytes: disk.free_bytes,
        disk_inode_free_pct: disk.inode_free_pct,
        // Rates are averaged over the window; drops and errors are summed, because they are events
        // rather than levels — "3 packets dropped in this window" is the fact, and averaging it to
        // one per sub-sample loses the count.
        nic_rx_bps: subs.iter().map(|s| s.nic_rx_bps).sum::<u64>() / subs.len() as u64,
        nic_tx_bps: subs.iter().map(|s| s.nic_tx_bps).sum::<u64>() / subs.len() as u64,
        nic_rx_drop: subs.iter().map(|s| s.nic_rx_drop).sum(),
        nic_tx_drop: subs.iter().map(|s| s.nic_tx_drop).sum(),
        nic_err: subs.iter().map(|s| s.nic_err).sum(),
        conntrack_count: read_u64_file("/proc/sys/net/netfilter/nf_conntrack_count"),
        uptime_secs: read_uptime(),
    }
}

/// The four processes we put on the machine.
///
/// wg is reported with everything `None` when the backend is in-kernel, which is honest rather
/// than lazy: WireGuard has no process then, its cost lands in softirq, and inventing a number for
/// it would be inventing a number. On a userspace backend (wireguard-go / boringtun) there is a
/// real process and it is measured like any other.
fn processes(subs: &[Sub], raw: &Raw) -> Vec<ProcessSample> {
    let n = subs.len() as f32;
    let cpu_of = |name: &str| -> Option<f32> {
        let sum: f32 = subs
            .iter()
            .map(|s| s.proc_cpu_pct.get(name).copied().unwrap_or(0.0))
            .sum();
        raw.proc_cpu.contains_key(name).then_some(sum / n)
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
    HostFacts {
        kernel: read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_default(),
        cores: num_cores(),
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
        conntrack_max: read_u64_file("/proc/sys/net/netfilter/nf_conntrack_max"),
        sysctl_managed: installer_set_congestion_control(),
        arch: std::env::consts::ARCH.to_owned(),
        os_pretty: os_pretty_name(),
        virt: detect_virt(),
        rmem_max: read_u64_file("/proc/sys/net/core/rmem_max").unwrap_or(0),
        wmem_max: read_u64_file("/proc/sys/net/core/wmem_max").unwrap_or(0),
        somaxconn: read_u64_file("/proc/sys/net/core/somaxconn").unwrap_or(0),
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

fn read_raw() -> Raw {
    let stat = fs::read_to_string("/proc/stat").unwrap_or_default();
    let pids = find_pids();
    Raw {
        at: now_secs(),
        btime: parse_btime(&stat).unwrap_or(0),
        cpu: parse_cpu(&stat),
        net: read_net(),
        oom_kills: read_vmstat_oom(),
        proc_cpu: read_proc_cpu(&pids),
        pids,
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_cpu(stat: &str) -> CpuTimes {
    // The aggregate line: `cpu  user nice system idle iowait irq softirq steal guest guest_nice`
    let Some(line) = stat.lines().find(|l| l.starts_with("cpu ")) else {
        return CpuTimes::default();
    };
    let f: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    let get = |i: usize| f.get(i).copied().unwrap_or(0);
    CpuTimes {
        // nice folded into user: it is user time, and splitting it out would add a fourth class to
        // a display whose whole point is three.
        user: get(0) + get(1),
        // irq folded into system, softirq kept apart. That asymmetry is the point of this split:
        // softirq is where packet forwarding lands, and it is the one class whose growth changes
        // what an operator should do.
        system: get(2) + get(5),
        softirq: get(6),
        steal: get(7),
        total: f.iter().sum(),
    }
}

fn parse_btime(stat: &str) -> Option<i64> {
    stat.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse().ok())
}

fn read_btime() -> Option<i64> {
    parse_btime(&fs::read_to_string("/proc/stat").ok()?)
}

struct MemInfo {
    total: u64,
    available: u64,
    swap_used: u64,
}

fn read_meminfo() -> MemInfo {
    let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let kb = |key: &str| -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    let swap_total = kb("SwapTotal:");
    let swap_free = kb("SwapFree:");
    MemInfo {
        total: kb("MemTotal:"),
        // MemAvailable, not MemFree. On any machine with a page cache MemFree is always small, and
        // judging by it reports every healthy machine as short of memory.
        available: kb("MemAvailable:"),
        swap_used: swap_total.saturating_sub(swap_free),
    }
}

fn read_vmstat_oom() -> u64 {
    fs::read_to_string("/proc/vmstat")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("oom_kill "))
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(0)
}

fn read_loadavg() -> f32 {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|t| t.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0.0)
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
        };
    };
    if unsafe { libc::statvfs(path.as_ptr(), &mut buf) } != 0 {
        return DiskInfo {
            total_bytes: 0,
            free_bytes: 0,
            inode_free_pct: 100.0,
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
    }
}

/// The interface carrying the default route.
///
/// Summing every interface would count wg0 and any tun on top of the physical one, reporting a
/// machine's own traffic two or three times.
fn main_interface() -> Option<String> {
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
    }

    #[test]
    fn reads_btime() {
        assert_eq!(parse_btime(STAT), Some(1_700_000_000));
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
            },
            net: NetCounters {
                rx_bytes: 1_000_000,
                tx_bytes: 500_000,
                ..Default::default()
            },
            oom_kills: 3,
            proc_cpu: BTreeMap::new(),
            pids: BTreeMap::new(),
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
            },
            net: NetCounters {
                rx_bytes: 200,
                tx_bytes: 100,
                ..Default::default()
            },
            oom_kills: 0,
            proc_cpu: BTreeMap::new(),
            pids: BTreeMap::new(),
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
            },
            net: NetCounters {
                rx_bytes: 1_000_000,
                ..Default::default()
            },
            oom_kills: 0,
            proc_cpu: BTreeMap::new(),
            pids: BTreeMap::new(),
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
            },
            net: NetCounters {
                rx_bytes: 900_000,
                ..Default::default()
            },
            oom_kills: 0,
            proc_cpu: BTreeMap::new(),
            pids: BTreeMap::new(),
        };
        let sub = difference(&before, &after);
        assert!(!sub.gap, "not a reboot — btime is unchanged");
        assert_eq!(sub.cpu_user_pct, 0.0);
        assert_eq!(sub.nic_rx_bps, 0);
    }

    /// The peak must be over the per-sub-sample sum, not the sum of per-class peaks: three maxima
    /// that never happened together can add past 100 on a machine that was never saturated.
    #[test]
    fn peak_is_the_worst_moment_not_the_sum_of_worst_classes() {
        let sub = |u: f32, s: f32, i: f32| Sub {
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
        };
        let subs = vec![
            sub(60.0, 5.0, 5.0),
            sub(5.0, 60.0, 5.0),
            sub(5.0, 5.0, 60.0),
        ];
        let sample = aggregate(&subs, Path::new("/tmp"), 1000, 30);
        // Each class peaks at 60; summing those gives 180. The real worst moment is 70.
        assert!(sample.cpu_peak_pct <= 100.0);
        assert!(
            (sample.cpu_peak_pct - 70.0).abs() < 0.01,
            "got {}",
            sample.cpu_peak_pct
        );
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
