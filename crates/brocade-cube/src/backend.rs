use crate::{
    config::Config,
    files,
    observation::Observations,
    records::{self, Record},
    rpc::Rpc,
    Result,
};
use brocade_deployment::cvm::*;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};

/// An in-process Cube backend. Clones share one bounded execution queue and lock.
#[derive(Clone)]
pub struct Backend(pub(crate) Arc<Owner>);

pub(crate) struct Owner {
    pub(crate) inner: Arc<Inner>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.inner.stopping.store(true, Ordering::Release);
    }
}

pub(crate) struct Inner {
    pub(crate) cfg: Config,
    pub(crate) images: BTreeMap<String, Value>,
    pub(crate) root: PathBuf,
    pub(crate) rpc: Rpc,
    pub(crate) runtime: tokio::runtime::Runtime,
    pub(crate) http: reqwest::blocking::Client,
    pub(crate) state: Mutex<State>,
    pub(crate) observations: Mutex<Observations>,
    pub(crate) observing: AtomicBool,
    pub(crate) stopping: AtomicBool,
    pub(crate) quota_supported: AtomicBool,
    pub(crate) network_mutation: tokio::sync::Mutex<()>,
    pub(crate) guest_io: Mutex<std::collections::BTreeSet<PathBuf>>,
    pub(crate) guest_recovery: Mutex<()>,
    _lock: Option<File>,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) records: BTreeMap<String, Record>,
    pub(crate) active: Option<InstanceDesired>,
    pub(crate) pending: BTreeMap<String, InstanceDesired>,
    order: VecDeque<String>,
    busy: bool,
}

impl Backend {
    pub fn open(config: &Path, state: &Path) -> Result<Self> {
        files::private_dir(state)?;
        let lock = files::lock_directory(state)?;
        let records = records::load(state)?;
        Self::connect(config, state.to_owned(), records, Some(lock))
    }

    /// Read-only readiness check; never claims execution records or starts workers.
    pub fn inspect(config: &Path) -> Result<Capabilities> {
        Self::connect(config, PathBuf::new(), BTreeMap::new(), None)?.inventory()
    }

    /// Prove the drained state while the managed Agent is stopped. The directory
    /// lock excludes another executor; stopped and unconfirmed instances also block.
    pub fn inspect_empty(config: &Path, state: &Path) -> Result<()> {
        let _lock = if state
            .try_exists()
            .map_err(|_| "Cube state cannot be inspected")?
        {
            let lock = files::lock_directory(state)?;
            if records::load(state)?.values().any(|record| !record.deleted) {
                return Err(
                    "CVM mode switching requires all instances to be deleted and cleanup confirmed"
                        .into(),
                );
            }
            Some(lock)
        } else {
            None
        };
        let backend = Self::connect(config, PathBuf::new(), BTreeMap::new(), None)?;
        let response = backend
            .0
            .inner
            .cube_timeout("List", json!({}), Duration::from_secs(10))?;
        if !crate::array(&response, "items").is_empty() {
            return Err(
                "Cube still has instances; drain the node before switching virtualization".into(),
            );
        }
        Ok(())
    }

    fn connect(
        config: &Path,
        state: PathBuf,
        records: BTreeMap<String, Record>,
        lock: Option<File>,
    ) -> Result<Self> {
        let (cfg, images) = Config::load(config)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "Cube RPC runtime unavailable")?;
        let rpc = {
            let _guard = runtime.enter();
            Rpc::new(cfg.cubelet_socket.clone())?
        };
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| "Cube inspection client unavailable")?;
        Ok(Self(Arc::new(Owner {
            inner: Arc::new(Inner {
                cfg,
                images,
                root: state,
                rpc,
                runtime,
                http,
                state: Mutex::new(State {
                    records,
                    ..State::default()
                }),
                observations: Mutex::new(Observations::default()),
                observing: AtomicBool::new(false),
                stopping: AtomicBool::new(false),
                quota_supported: AtomicBool::new(false),
                network_mutation: tokio::sync::Mutex::new(()),
                guest_io: Mutex::new(std::collections::BTreeSet::new()),
                guest_recovery: Mutex::new(()),
                _lock: lock,
            }),
        })))
    }

    pub fn inventory(&self) -> Result<Capabilities> {
        let inner = &self.0.inner;
        inner.cube_timeout("List", json!({}), Duration::from_secs(10))?;
        if inner._lock.is_some() {
            if let Err(error) = inner.recover_guest_io() {
                eprintln!("CVM pending Guest cleanup: {error}");
            }
        }
        let capabilities = inner.native_capabilities()?;
        if capabilities.stable_storage {
            inner.storage_contract()?;
        }
        capabilities.validate()?;
        Ok(capabilities)
    }

    pub fn submit(&self, desired: &InstanceDesired) -> Result<InstanceReport> {
        if !valid_id(&desired.id)
            || !valid_id(&desired.operation_id)
            || desired.generation == 0
            || desired.spec_sha256
                != brocade_core::hash::sha256_hex(
                    &serde_json::to_vec(&desired.spec)
                        .map_err(|_| "CVM frozen specification serialization failed")?,
                )
        {
            return Err("invalid Cube desired identity".into());
        }
        if desired.detached_interfaces.len() > MAX_INTERFACES
            || desired
                .detached_interfaces
                .iter()
                .any(|id| !valid_id(id) || desired.spec.interfaces.iter().any(|nic| &nic.id == id))
            || desired
                .detached_interfaces
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != desired.detached_interfaces.len()
        {
            return Err("invalid instance-scoped detached interface assignment".into());
        }
        let inner = &self.0.inner;
        let mut state = inner.state()?;
        if let Some(record) = state.records.get(&desired.id) {
            if desired.generation < record.generation
                || (desired.generation == record.generation
                    && desired.spec_sha256 != record.spec_sha256)
            {
                return Ok(report(
                    desired,
                    ApplyStatus::Failed,
                    Some("拒绝过期或同代次摘要变化的规格"),
                ));
            }
        }
        for newer in [state.active.as_ref(), state.pending.get(&desired.id)]
            .into_iter()
            .flatten()
        {
            if newer.id == desired.id
                && (desired.generation < newer.generation
                    || (desired.generation == newer.generation
                        && (desired.spec_sha256 != newer.spec_sha256
                            || desired.operation_id != newer.operation_id)))
            {
                return Ok(report(
                    desired,
                    ApplyStatus::Failed,
                    Some("拒绝替换已接收的新版本任务"),
                ));
            }
        }
        let result = state
            .records
            .get(&desired.id)
            .and_then(|r| r.report.as_ref())
            .filter(|r| {
                r.generation == desired.generation && r.operation_id == desired.operation_id
            })
            .cloned()
            .unwrap_or_else(|| report(desired, ApplyStatus::Pending, None));
        if state
            .active
            .as_ref()
            .is_some_and(|a| a.id == desired.id && a.generation == desired.generation)
        {
            return Ok(result);
        }
        if !state.pending.contains_key(&desired.id)
            && state.pending.len() >= MAX_INSTANCES_PER_MACHINE
        {
            return Ok(result);
        }
        if !state.records.contains_key(&desired.id) {
            if state.records.len() >= 8192 {
                return Ok(report(
                    desired,
                    ApplyStatus::Failed,
                    Some("本机执行记录达到上限"),
                ));
            }
            state.records.insert(
                desired.id.clone(),
                Record {
                    instance: desired.id.clone(),
                    ..Record::default()
                },
            );
        }
        if !state.pending.contains_key(&desired.id) {
            state.order.push_back(desired.id.clone());
        }
        state.pending.insert(desired.id.clone(), desired.clone());
        if !state.busy {
            state.busy = true;
            let worker = inner.clone();
            if std::thread::Builder::new()
                .name("cvm-executor".into())
                .spawn(move || worker.drain())
                .is_err()
            {
                state.busy = false;
                return Err("cannot start Cube execution worker".into());
            }
        }
        Ok(result)
    }

    pub(crate) fn inner(&self) -> &Arc<Inner> {
        &self.0.inner
    }

    /// Wait for submitted intents at a controlled shutdown or acceptance boundary.
    pub fn wait_idle(&self, timeout: Duration) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let state = self.inner().state()?;
            if !state.busy && state.active.is_none() && state.pending.is_empty() {
                return Ok(());
            }
            drop(state);
            if std::time::Instant::now() >= deadline {
                return Err("Cube execution did not reach an idle boundary".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Inner {
    pub(crate) fn state(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| "Cube execution state is unavailable".into())
    }

    pub(crate) fn cube(&self, method: &str, value: Value) -> Result<Value> {
        self.cube_timeout(method, value, Duration::from_secs(240))
    }

    pub(crate) fn cube_timeout(
        &self,
        method: &str,
        value: Value,
        timeout: Duration,
    ) -> Result<Value> {
        self.runtime.block_on(async {
            tokio::time::timeout(timeout, self.rpc.cube(method, value))
                .await
                .map_err(|_| "Cube RPC deadline exceeded")?
        })
    }

    pub(crate) fn task(&self, method: &str, value: Value, timeout: Duration) -> Result<Value> {
        self.runtime.block_on(async {
            tokio::time::timeout(timeout, self.rpc.task(method, value))
                .await
                .map_err(|_| "Guest task RPC deadline exceeded")?
        })
    }

    fn drain(self: Arc<Self>) {
        loop {
            let next = (|| -> Result<Option<(InstanceDesired, Record)>> {
                let mut state = self.state()?;
                if self.stopping.load(Ordering::Acquire) || state.order.is_empty() {
                    state.busy = false;
                    state.active = None;
                    return Ok(None);
                }
                let id = state.order.pop_front().ok_or("Cube queue entry missing")?;
                let desired = state
                    .pending
                    .remove(&id)
                    .ok_or("Cube pending task missing")?;
                let record = state
                    .records
                    .get(&id)
                    .cloned()
                    .ok_or("Cube execution record missing")?;
                state.active = Some(desired.clone());
                Ok(Some((desired, record)))
            })();
            let (desired, mut record) = match next {
                Ok(Some(next)) => next,
                Ok(None) => return,
                Err(error) => {
                    eprintln!("CVM executor: {error}");
                    return;
                }
            };
            let result = self
                .reconcile(&mut record, &desired)
                .unwrap_or_else(|error| report(&desired, ApplyStatus::Unknown, Some(&error)));
            if result.status == ApplyStatus::Applied && result.power == Some(PowerState::Running) {
                record.last_interfaces = result.interfaces.clone();
            }
            record.report = Some(result);
            record.generation = desired.generation;
            record.spec_sha256 = desired.spec_sha256.clone();
            if record.save(&self.root).is_err() {
                record.report = Some(report(
                    &desired,
                    ApplyStatus::Unknown,
                    Some("执行回执未能持久保存；预约保留"),
                ));
            }
            match self.state() {
                Ok(mut state) => {
                    state.records.insert(record.instance.clone(), record);
                    state.active = None;
                }
                Err(error) => {
                    eprintln!("CVM executor: {error}");
                    return;
                }
            }
        }
    }

    pub(crate) fn list(&self, id: &str) -> Result<Vec<Value>> {
        let response = self.cube(
            "List",
            json!({"filter":{"label_selector":{"brocade.cvm.instance":id}}}),
        )?;
        Ok(crate::array(&response, "items")
            .iter()
            .filter(|b| crate::text(&b["labels"], "brocade.cvm.instance") == id)
            .cloned()
            .collect())
    }

    pub(crate) fn runtime_owned(&self, record: &Record) -> Result<()> {
        if !valid_runtime(&record.runtime) {
            return Err("Cube runtime identity invalid".into());
        }
        let response = self.cube("List", json!({"id":record.runtime}))?;
        for item in crate::array(&response, "items") {
            if crate::text(item, "id") == record.runtime
                && (crate::text(&item["labels"], "brocade.cvm.instance") != record.instance
                    || crate::text(&item["labels"], "brocade.cvm.spec_sha256")
                        != record.launch_sha256.as_str())
            {
                return Err("Cube runtime ownership does not match".into());
            }
        }
        Ok(())
    }
}

pub(crate) fn valid_runtime(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 150
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(crate) fn running(value: &Value) -> bool {
    crate::array(value, "containers")
        .iter()
        .any(|c| crate::number(c, "state") == 1)
}

pub fn report(
    desired: &InstanceDesired,
    status: ApplyStatus,
    error: Option<&str>,
) -> InstanceReport {
    InstanceReport {
        id: desired.id.clone(),
        operation_id: desired.operation_id.clone(),
        generation: desired.generation,
        spec_sha256: desired.spec_sha256.clone(),
        status,
        power: None,
        runtime_id: None,
        volume_id: None,
        access_address: None,
        interfaces: vec![],
        network: None,
        detached_interfaces: vec![],
        network_absent: false,
        volume_retained: false,
        volume_deleted: false,
        error: error.map(str::to_owned),
        restart_epoch: desired.restart_epoch,
        reinstall_epoch: desired.reinstall_epoch,
    }
}
