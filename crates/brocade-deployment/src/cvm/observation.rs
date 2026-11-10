use super::*;

/// Cumulative counters from inside this VM, not from its host or VMM process.
/// Rates require two samples from the same Guest boot and network device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<GuestDiagnostics>,
    pub observed_at_unix_millis: u64,
    pub boot_id: String,
    pub uptime_millis: u64,
    pub cpu_total_ticks: u64,
    pub cpu_busy_ticks: u64,
    pub memory_total_bytes: u64,
    pub memory_available_bytes: u64,
    pub root_total_bytes: u64,
    pub root_used_bytes: u64,
    pub interfaces: Vec<InterfaceCounters>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceCounters {
    pub id: String,
    pub device: String,
    pub ifindex: u32,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    #[serde(default)]
    pub rx_packets: Option<u64>,
    #[serde(default)]
    pub tx_packets: Option<u64>,
    #[serde(default)]
    pub rx_dropped: Option<u64>,
    #[serde(default)]
    pub tx_dropped: Option<u64>,
    #[serde(default)]
    pub rx_errors: Option<u64>,
    #[serde(default)]
    pub tx_errors: Option<u64>,
    #[serde(default)]
    pub carrier: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuCounters {
    /// -1 is the aggregate; other values identify a Guest CPU.
    pub cpu: i32,
    pub ticks: [u64; 8],
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PressureReading {
    pub resource: String,
    pub some_basis_points: u32,
    pub full_basis_points: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskCounters {
    pub device: String,
    pub reads: u64,
    pub writes: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub read_millis: u64,
    pub write_millis: u64,
    pub busy_millis: u64,
    pub weighted_millis: u64,
    pub in_flight: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestDiagnostics {
    #[serde(default)]
    pub cpus: Vec<CpuCounters>,
    pub load_milli: Option<[u32; 3]>,
    pub running_processes: Option<u32>,
    pub total_processes: Option<u32>,
    #[serde(default)]
    pub memory: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub vm: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub tcp: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub pressure: Vec<PressureReading>,
    pub inode_total: Option<u64>,
    pub inode_free: Option<u64>,
    pub disk_io: Option<DiskCounters>,
    #[serde(default)]
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceProbeTarget {
    pub id: String,
    pub interface_id: String,
    pub host: String,
    pub port: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationTarget {
    pub id: String,
    pub generation: u64,
    pub power: PowerState,
    #[serde(default)]
    pub services: Vec<ServiceProbeTarget>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedRuntimeState {
    Unknown,
    Running,
    Stopped,
    Missing,
    Transitioning,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    Unknown,
    Reachable,
    Unreachable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceProbeResult {
    pub id: String,
    pub state: Reachability,
    pub latency_millis: Option<u32>,
}
/// Operational observations have their own cadence and do not complete an intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceObservation {
    pub id: String,
    pub generation: u64,
    pub observed_at_unix_millis: u64,
    pub runtime_id: Option<String>,
    pub runtime_state: ObservedRuntimeState,
    pub guest_state: Reachability,
    pub ssh_state: Reachability,
    pub metrics: Option<GuestMetrics>,
    pub error: Option<String>,
    #[serde(default)]
    pub services: Vec<ServiceProbeResult>,
}

impl GuestMetrics {
    pub fn validate_observation(&self, now_millis: u64) -> Result<(), &'static str> {
        if self.interfaces.len() > MAX_INTERFACES
            || self.interfaces.iter().any(|n| {
                !valid_id(&n.id)
                    || n.device.is_empty()
                    || n.device.len() > 15
                    || n.device.contains('/')
                    || n.ifindex == 0
            })
        {
            return Err("CVM monitoring interface identity is invalid");
        }
        if let Some(d) = &self.diagnostics {
            const MAX_COUNTER: u64 = (1 << 53) - 1;
            let mut cores = BTreeSet::new();
            if d.cpus.len() > 33
                || d.pressure.len() > 3
                || d.memory.len() > 24
                || d.vm.len() > 16
                || d.tcp.len() > 24
                || d.unsupported.len() > 16
                || d.unsupported.iter().any(|v| v.len() > 80)
                || d.cpus.iter().any(|c| {
                    c.cpu < -1
                        || c.cpu > 4095
                        || !cores.insert(c.cpu)
                        || c.ticks.iter().any(|&n| n > MAX_COUNTER / 8)
                })
                || d.memory
                    .iter()
                    .chain(d.vm.iter())
                    .chain(d.tcp.iter())
                    .any(|(k, &v)| k.len() > 40 || v > MAX_COUNTER)
                || d.disk_io.as_ref().is_some_and(|d| {
                    d.device.len() > 64
                        || [
                            d.reads,
                            d.writes,
                            d.read_bytes,
                            d.write_bytes,
                            d.read_millis,
                            d.write_millis,
                            d.busy_millis,
                            d.weighted_millis,
                            d.in_flight,
                        ]
                        .iter()
                        .any(|&n| n > MAX_COUNTER)
                })
                || [d.inode_total, d.inode_free]
                    .iter()
                    .flatten()
                    .any(|&v| v > MAX_COUNTER)
                || d.inode_free
                    .zip(d.inode_total)
                    .is_some_and(|(free, total)| free > total)
                || d.pressure.iter().any(|p| {
                    !["cpu", "memory", "io"].contains(&p.resource.as_str())
                        || p.some_basis_points > 10_000
                        || p.full_basis_points.is_some_and(|v| v > 10_000)
                })
            {
                return Err("CVM diagnostics exceed supported bounds");
            }
        }
        const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
        if self.observed_at_unix_millis < now_millis.saturating_sub(3_600_000)
            || self.observed_at_unix_millis > now_millis.saturating_add(60_000)
            || self.boot_id.len() != 36
            || self
                .boot_id
                .bytes()
                .any(|c| !c.is_ascii_hexdigit() && c != b'-')
            || self.uptime_millis == 0
            || self.cpu_busy_ticks > self.cpu_total_ticks
            || self.memory_total_bytes == 0
            || self.memory_available_bytes > self.memory_total_bytes
            || self.root_total_bytes == 0
            || self.root_used_bytes > self.root_total_bytes
            || self.interfaces.len() > MAX_INTERFACES
            || [
                self.uptime_millis,
                self.cpu_total_ticks,
                self.cpu_busy_ticks,
                self.memory_total_bytes,
                self.root_total_bytes,
            ]
            .iter()
            .any(|&v| v > MAX_SAFE_INTEGER)
        {
            return Err("CVM Guest monitoring observation is invalid");
        }
        let mut ids = BTreeSet::new();
        for nic in &self.interfaces {
            if nic.ifindex == 0
                || nic.rx_bytes > MAX_SAFE_INTEGER
                || nic.tx_bytes > MAX_SAFE_INTEGER
                || [
                    nic.rx_packets,
                    nic.tx_packets,
                    nic.rx_dropped,
                    nic.tx_dropped,
                    nic.rx_errors,
                    nic.tx_errors,
                ]
                .iter()
                .flatten()
                .any(|&n| n > MAX_SAFE_INTEGER)
                || !ids.insert(&nic.id)
            {
                return Err("CVM monitoring interface identity is invalid");
            }
        }
        Ok(())
    }
}
