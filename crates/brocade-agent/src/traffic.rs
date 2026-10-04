//! Durable accounting for the physical interface carrying the default route.
//!
//! Linux resets an interface's counters on boot and an Agent process may restart between two
//! reports. Publishing the raw sysfs value would therefore make a reboot look like negative
//! traffic. This module keeps a private logical counter in the Agent state directory, advances it
//! from kernel deltas, and persists every sample before it can be reported.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use brocade_deployment::protocol::NodeTrafficReading;
use serde::{Deserialize, Serialize};

const STATE_FILE: &str = "traffic-meter.json";
const STATE_VERSION: u32 = 1;
static METER_LOCK: Mutex<()> = Mutex::new(());
const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(30);

/// A derived view of the last *durably committed* reading, never another accounting authority.
/// Its lock protects only a small clone, not sysfs access, file I/O or the meter writer's lock.
#[derive(Default)]
pub(crate) struct TrafficSnapshot {
    committed: Mutex<Option<(Instant, NodeTrafficReading)>>,
}

impl TrafficSnapshot {
    pub(crate) fn latest(&self) -> Option<NodeTrafficReading> {
        self.latest_at(Instant::now())
    }

    fn latest_at(&self, now: Instant) -> Option<NodeTrafficReading> {
        self.committed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|(sampled_at, _)| {
                now.saturating_duration_since(*sampled_at) <= SNAPSHOT_MAX_AGE
            })
            .map(|(_, reading)| reading.clone())
    }

    fn sample(
        &self,
        sample: impl FnOnce() -> Result<NodeTrafficReading, String>,
    ) -> Result<(), String> {
        // Age includes persistence time. A stuck disk must not make an old observation fresh
        // merely because its fsync eventually returns. Failed commits retain the old snapshot.
        let sampled_at = Instant::now();
        let reading = sample()?;
        *self
            .committed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((sampled_at, reading));
        Ok(())
    }
}

/// The daemon has exactly one durable interface-meter writer, independent of telemetry and
/// runtime reporting. Shutdown wakes that writer for a final serialized sample before joining it.
pub(crate) struct TrafficWorker {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: JoinHandle<Result<(), String>>,
}

impl TrafficWorker {
    pub(crate) fn spawn(
        state_dir: PathBuf,
        snapshot: Arc<TrafficSnapshot>,
    ) -> Result<Self, String> {
        Self::spawn_with_sampler(snapshot, SAMPLE_INTERVAL, move || sample(&state_dir))
    }

    fn spawn_with_sampler(
        snapshot: Arc<TrafficSnapshot>,
        interval: Duration,
        mut sample: impl FnMut() -> Result<NodeTrafficReading, String> + Send + 'static,
    ) -> Result<Self, String> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("traffic-meter".to_owned())
            .spawn(move || {
                let mut tick = Instant::now();
                loop {
                    let started = Instant::now();
                    crate::each_round("traffic", || {
                        if let Err(error) = snapshot.sample(&mut sample) {
                            eprintln!("traffic: {error}");
                        }
                    });
                    let now = Instant::now();
                    crate::warn_slow_round("traffic", tick, started, now);
                    tick = crate::next_periodic_tick(tick, interval, now);
                    let (lock, ready) = &*worker_stop;
                    let stop = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let (stop, _) = ready
                        .wait_timeout_while(
                            stop,
                            tick.saturating_duration_since(Instant::now()),
                            |stop| !*stop,
                        )
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if *stop {
                        drop(stop);
                        return snapshot
                            .sample(&mut sample)
                            .map_err(|error| format!("traffic final sample: {error}"));
                    }
                }
            })
            .map_err(|error| format!("cannot spawn traffic-meter thread: {error}"))?;
        Ok(Self { stop, thread })
    }

    pub(crate) fn finish(self) -> Result<(), String> {
        let (lock, ready) = &*self.stop;
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        ready.notify_one();
        self.thread
            .join()
            .map_err(|_| "traffic-meter thread panicked during shutdown".to_owned())?
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawTraffic {
    interface: String,
    boot_id: String,
    rx_bytes: u64,
    tx_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrafficMeterState {
    version: u32,
    meter_id: String,
    sequence: u64,
    interface: String,
    boot_id: String,
    raw_rx_bytes: u64,
    raw_tx_bytes: u64,
    total_rx_bytes: u64,
    total_tx_bytes: u64,
    discontinuities: u64,
}

impl TrafficMeterState {
    fn reading(&self) -> NodeTrafficReading {
        NodeTrafficReading {
            meter_id: self.meter_id.clone(),
            sequence: self.sequence,
            interface: self.interface.clone(),
            boot_id: self.boot_id.clone(),
            rx_bytes: self.total_rx_bytes,
            tx_bytes: self.total_tx_bytes,
            discontinuities: self.discontinuities,
        }
    }
}

/// Sample, durably advance, and return the logical byte meter.
///
/// The daemon calls this only from its meter worker; one-shot commands use it synchronously.
/// Retain writer serialization as a safety boundary: an atomic rename alone cannot prevent two
/// callers from advancing the same previous raw value and letting the older rename win.
pub(crate) fn sample(state_dir: &Path) -> Result<NodeTrafficReading, String> {
    let _guard = METER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let raw = read_raw()?;
    sample_locked(state_dir, raw)
}

fn sample_locked(state_dir: &Path, raw: RawTraffic) -> Result<NodeTrafficReading, String> {
    sample_with_writer(state_dir, raw, crate::fsutil::atomic_write_private)
}

fn sample_with_writer(
    state_dir: &Path,
    raw: RawTraffic,
    write: impl FnOnce(&Path, &[u8]) -> Result<(), String>,
) -> Result<NodeTrafficReading, String> {
    let path = state_dir.join(STATE_FILE);
    let previous = match fs::read_to_string(&path) {
        Ok(text) => {
            let state: TrafficMeterState = serde_json::from_str(&text)
                .map_err(|error| format!("decode {}: {error}", path.display()))?;
            validate_state(&state)?;
            Some(state)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let state = match previous {
        Some(previous) => advance(previous, raw)?,
        None => TrafficMeterState {
            version: STATE_VERSION,
            meter_id: new_meter_id()?,
            sequence: 1,
            interface: raw.interface,
            boot_id: raw.boot_id,
            // Installation cannot reconstruct traffic that preceded the first sample. Establish a
            // baseline instead of claiming all bytes since boot as Brocade-observed traffic.
            raw_rx_bytes: raw.rx_bytes,
            raw_tx_bytes: raw.tx_bytes,
            total_rx_bytes: 0,
            total_tx_bytes: 0,
            discontinuities: 0,
        },
    };
    let encoded = serde_json::to_vec(&state).map_err(|error| error.to_string())?;
    write(&path, &encoded)?;
    Ok(state.reading())
}

fn advance(mut state: TrafficMeterState, raw: RawTraffic) -> Result<TrafficMeterState, String> {
    state.sequence = state
        .sequence
        .checked_add(1)
        .ok_or_else(|| "traffic meter sequence exhausted".to_owned())?;

    if state.boot_id != raw.boot_id {
        // Kernel counters start at zero on a new boot. The first reading includes everything since
        // that boot, including traffic before the Agent service started, so it can be added in
        // full. Bytes between the last durable pre-reboot sample and power-off remain unknowable.
        state.total_rx_bytes = state
            .total_rx_bytes
            .checked_add(raw.rx_bytes)
            .ok_or_else(|| "traffic RX total overflow".to_owned())?;
        state.total_tx_bytes = state
            .total_tx_bytes
            .checked_add(raw.tx_bytes)
            .ok_or_else(|| "traffic TX total overflow".to_owned())?;
        state.discontinuities = state
            .discontinuities
            .checked_add(1)
            .ok_or_else(|| "traffic discontinuity counter exhausted".to_owned())?;
    } else if state.interface != raw.interface {
        // The two devices may represent the same packets or entirely different paths. Neither
        // adding the new raw value nor subtracting the old one is defensible; retain the known
        // logical total and establish a new baseline.
        state.discontinuities = state
            .discontinuities
            .checked_add(1)
            .ok_or_else(|| "traffic discontinuity counter exhausted".to_owned())?;
    } else {
        let rx_delta = raw.rx_bytes.checked_sub(state.raw_rx_bytes);
        let tx_delta = raw.tx_bytes.checked_sub(state.raw_tx_bytes);
        if rx_delta.is_none() || tx_delta.is_none() {
            state.discontinuities = state
                .discontinuities
                .checked_add(1)
                .ok_or_else(|| "traffic discontinuity counter exhausted".to_owned())?;
        }
        // A reset in one direction does not make the monotonic direction unknowable.
        state.total_rx_bytes = state
            .total_rx_bytes
            .checked_add(rx_delta.unwrap_or(0))
            .ok_or_else(|| "traffic RX total overflow".to_owned())?;
        state.total_tx_bytes = state
            .total_tx_bytes
            .checked_add(tx_delta.unwrap_or(0))
            .ok_or_else(|| "traffic TX total overflow".to_owned())?;
    }

    state.interface = raw.interface;
    state.boot_id = raw.boot_id;
    state.raw_rx_bytes = raw.rx_bytes;
    state.raw_tx_bytes = raw.tx_bytes;
    Ok(state)
}

fn read_raw() -> Result<RawTraffic, String> {
    let interface = crate::load::main_interface()
        .ok_or_else(|| "no default-route interface is available".to_owned())?;
    if !valid_interface(&interface) {
        return Err(format!("invalid default-route interface {interface:?}"));
    }
    let root = Path::new("/sys/class/net")
        .join(&interface)
        .join("statistics");
    let read_counter = |name: &str| -> Result<u64, String> {
        let path = root.join(name);
        fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?
            .trim()
            .parse::<u64>()
            .map_err(|error| format!("parse {}: {error}", path.display()))
    };
    let boot_path = Path::new("/proc/sys/kernel/random/boot_id");
    let boot_id = fs::read_to_string(boot_path)
        .map_err(|error| format!("read {}: {error}", boot_path.display()))?
        .trim()
        .to_owned();
    if !valid_boot_id(&boot_id) {
        return Err("kernel boot id has an invalid shape".to_owned());
    }
    Ok(RawTraffic {
        interface,
        boot_id,
        rx_bytes: read_counter("rx_bytes")?,
        tx_bytes: read_counter("tx_bytes")?,
    })
}

fn new_meter_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn validate_state(state: &TrafficMeterState) -> Result<(), String> {
    if state.version != STATE_VERSION {
        return Err(format!(
            "unsupported traffic meter state version {}",
            state.version
        ));
    }
    if state.meter_id.len() != 32
        || !state
            .meter_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("traffic meter id has an invalid shape".to_owned());
    }
    if state.sequence == 0 {
        return Err("traffic meter sequence must be positive".to_owned());
    }
    if !valid_interface(&state.interface) || !valid_boot_id(&state.boot_id) {
        return Err("traffic meter identity has an invalid shape".to_owned());
    }
    Ok(())
}

fn valid_interface(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.contains('/')
        && value != "."
        && value != ".."
        && !value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_whitespace())
}

fn valid_boot_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use std::{env, fs, os::unix::fs::PermissionsExt, sync::mpsc};

    use super::*;

    fn raw(interface: &str, boot_id: &str, rx: u64, tx: u64) -> RawTraffic {
        RawTraffic {
            interface: interface.to_owned(),
            boot_id: boot_id.to_owned(),
            rx_bytes: rx,
            tx_bytes: tx,
        }
    }

    fn state(raw: RawTraffic) -> TrafficMeterState {
        TrafficMeterState {
            version: STATE_VERSION,
            meter_id: "0123456789abcdef0123456789abcdef".to_owned(),
            sequence: 1,
            interface: raw.interface,
            boot_id: raw.boot_id,
            raw_rx_bytes: raw.rx_bytes,
            raw_tx_bytes: raw.tx_bytes,
            total_rx_bytes: 100,
            total_tx_bytes: 200,
            discontinuities: 0,
        }
    }

    #[test]
    fn agent_restart_on_same_boot_continues_from_persisted_raw_counters() {
        let boot = "11111111-2222-3333-4444-555555555555";
        let next = advance(
            state(raw("eth0", boot, 1_000, 2_000)),
            raw("eth0", boot, 1_025, 2_075),
        )
        .unwrap();
        assert_eq!((next.total_rx_bytes, next.total_tx_bytes), (125, 275));
        assert_eq!(next.discontinuities, 0);
        assert_eq!(next.sequence, 2);
    }

    fn reading() -> NodeTrafficReading {
        state(raw(
            "eth0",
            "11111111-2222-3333-4444-555555555555",
            1_000,
            2_000,
        ))
        .reading()
    }

    #[test]
    fn a_stalled_writer_does_not_lock_runtime_reads_or_publish_uncommitted_bytes() {
        let snapshot = Arc::new(TrafficSnapshot::default());
        snapshot.sample(|| Ok(reading())).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let mut calls = 0;
        let worker = TrafficWorker::spawn_with_sampler(
            Arc::clone(&snapshot),
            Duration::from_secs(60),
            move || {
                calls += 1;
                if calls == 1 {
                    entered_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                }
                let mut next = reading();
                next.sequence += calls;
                next.rx_bytes += calls * 10;
                Ok(next)
            },
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (read_tx, read_rx) = mpsc::channel();
        let reader_snapshot = Arc::clone(&snapshot);
        let reader = thread::spawn(move || {
            read_tx.send(reader_snapshot.latest()).unwrap();
        });
        let during_write = read_rx.recv_timeout(Duration::from_secs(2));
        // Always release the fake fsync, even on failure, so a regression cannot hang the suite.
        resume_tx.send(()).unwrap();
        worker.finish().unwrap();
        reader.join().unwrap();
        assert_eq!(during_write.unwrap(), Some(reading()));
        let final_reading = snapshot.latest().unwrap();
        assert_eq!(
            final_reading.sequence, 3,
            "shutdown asks the same writer for one final sample"
        );
        assert_eq!(final_reading.rx_bytes, 120);
    }

    #[test]
    fn failed_samples_keep_the_committed_reading_and_old_readings_expire() {
        let snapshot = TrafficSnapshot::default();
        assert!(snapshot.latest().is_none());
        snapshot.sample(|| Ok(reading())).unwrap();
        let sampled_at = snapshot.committed.lock().unwrap().as_ref().unwrap().0;
        assert!(snapshot.sample(|| Err("fsync failed".to_owned())).is_err());
        assert_eq!(
            snapshot.latest_at(sampled_at + SNAPSHOT_MAX_AGE),
            Some(reading())
        );
        assert_eq!(
            snapshot.latest_at(sampled_at + SNAPSHOT_MAX_AGE + Duration::from_nanos(1)),
            None
        );
        assert_eq!(
            snapshot.committed.lock().unwrap().as_ref().unwrap().0,
            sampled_at,
            "a failed write cannot renew snapshot freshness"
        );
    }

    #[test]
    fn shutdown_reports_final_persistence_failure() {
        let snapshot = Arc::new(TrafficSnapshot::default());
        let (entered_tx, entered_rx) = mpsc::channel();
        let mut first = true;
        let worker = TrafficWorker::spawn_with_sampler(
            Arc::clone(&snapshot),
            Duration::from_secs(60),
            move || {
                if first {
                    first = false;
                    entered_tx.send(()).unwrap();
                    Ok(reading())
                } else {
                    Err("disk full".to_owned())
                }
            },
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            worker.finish(),
            Err("traffic final sample: disk full".to_owned())
        );
        assert_eq!(snapshot.latest(), Some(reading()));
    }

    #[test]
    fn retry_after_failed_or_uncertain_commit_never_loses_or_double_counts_known_bytes() {
        let boot = "11111111-2222-3333-4444-555555555555";
        for renamed in [false, true] {
            let dir = env::temp_dir().join(format!(
                "brocade-traffic-meter-retry-{}-{renamed}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            let snapshot = TrafficSnapshot::default();
            snapshot
                .sample(|| sample_locked(&dir, raw("eth0", boot, 1_000, 2_000)))
                .unwrap();
            let baseline = snapshot.latest().unwrap();
            let result = snapshot.sample(|| {
                sample_with_writer(&dir, raw("eth0", boot, 1_100, 2_200), |path, bytes| {
                    // A directory fsync can fail after rename, leaving an uncertain commit.
                    if renamed {
                        crate::fsutil::atomic_write_private(path, bytes)?;
                    }
                    Err("injected persistence failure".to_owned())
                })
            });
            assert!(result.is_err());
            assert_eq!(snapshot.latest(), Some(baseline.clone()));
            snapshot
                .sample(|| sample_locked(&dir, raw("eth0", boot, 1_150, 2_250)))
                .unwrap();
            let recovered = snapshot.latest().unwrap();
            assert_eq!(recovered.meter_id, baseline.meter_id);
            assert_eq!((recovered.rx_bytes, recovered.tx_bytes), (150, 250));
            assert_eq!(recovered.discontinuities, 0);
            assert_eq!(recovered.sequence, if renamed { 3 } else { 2 });
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn machine_reboot_preserves_total_and_adds_new_boot_counters() {
        let next = advance(
            state(raw(
                "eth0",
                "11111111-2222-3333-4444-555555555555",
                1_000,
                2_000,
            )),
            raw("eth0", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", 25, 75),
        )
        .unwrap();
        assert_eq!((next.total_rx_bytes, next.total_tx_bytes), (125, 275));
        assert_eq!(next.discontinuities, 1);
    }

    #[test]
    fn interface_replacement_does_not_double_count() {
        let boot = "11111111-2222-3333-4444-555555555555";
        let next = advance(
            state(raw("eth0", boot, 1_000, 2_000)),
            raw("ens5", boot, 80_000, 90_000),
        )
        .unwrap();
        assert_eq!((next.total_rx_bytes, next.total_tx_bytes), (100, 200));
        assert_eq!(next.discontinuities, 1);
    }

    #[test]
    fn one_counter_regression_keeps_the_other_known_delta() {
        let boot = "11111111-2222-3333-4444-555555555555";
        let next = advance(
            state(raw("eth0", boot, 1_000, 2_000)),
            raw("eth0", boot, 10, 2_075),
        )
        .unwrap();
        assert_eq!((next.total_rx_bytes, next.total_tx_bytes), (100, 275));
        assert_eq!(next.discontinuities, 1);
    }

    #[test]
    fn corrupt_state_is_not_silently_replaced() {
        let dir = env::temp_dir().join(format!(
            "brocade-traffic-meter-corrupt-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(STATE_FILE);
        fs::write(&path, b"not-json").unwrap();
        let result = sample_locked(
            &dir,
            raw("eth0", "11111111-2222-3333-4444-555555555555", 1, 2),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(path).unwrap(), b"not-json");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn first_sample_is_a_private_baseline_and_later_sample_survives_reload() {
        let dir = env::temp_dir().join(format!(
            "brocade-traffic-meter-persist-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let boot = "11111111-2222-3333-4444-555555555555";
        let first = sample_locked(&dir, raw("eth0", boot, 1_000, 2_000)).unwrap();
        assert_eq!((first.rx_bytes, first.tx_bytes), (0, 0));
        let second = sample_locked(&dir, raw("eth0", boot, 1_125, 2_275)).unwrap();
        assert_eq!((second.rx_bytes, second.tx_bytes), (125, 275));
        assert_eq!(first.meter_id, second.meter_id);
        assert_eq!(second.sequence, 2);
        let path = dir.join(STATE_FILE);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let on_disk: TrafficMeterState = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(on_disk.reading(), second);
        let _ = fs::remove_dir_all(dir);
    }
}
