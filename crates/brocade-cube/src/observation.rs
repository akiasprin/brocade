use crate::{
    backend::{running, Backend, Inner},
    records::Record,
    Result,
};
use brocade_deployment::cvm::*;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct Observations {
    targets: BTreeMap<String, (ObservationTarget, Instant)>,
    cache: BTreeMap<String, VecDeque<InstanceObservation>>,
}

#[derive(Clone, PartialEq, Eq)]
struct Binding {
    runtime: String,
    launch: String,
    interfaces: Vec<InterfaceObservation>,
}

fn binding(record: &Record) -> Option<Binding> {
    if record.runtime.is_empty() || record.deleted {
        return None;
    }
    Some(Binding {
        runtime: record.runtime.clone(),
        launch: record.launch_sha256.clone(),
        interfaces: record.last_interfaces.clone(),
    })
}

impl Backend {
    /// Collect assigned targets once without applying or completing an intent.
    pub fn collect_observations(&self) -> Result<()> {
        self.inner().observe_cycle()
    }

    pub fn observe(
        &self,
        targets: &[ObservationTarget],
        active_ids: &[String],
    ) -> Result<Vec<InstanceObservation>> {
        let active = active_ids.iter().collect::<BTreeSet<_>>();
        if targets.len() > 8
            || active.len() > MAX_INSTANCES_PER_MACHINE
            || active.len() != active_ids.len()
            || active.iter().any(|id| !valid_id(id))
        {
            return Err("Cube observation assignment exceeds limits".into());
        }
        for target in targets {
            if !valid_id(&target.id)
                || target.generation == 0
                || !active.contains(&target.id)
                || target.services.len() > 8
            {
                return Err("invalid Cube observation identity".into());
            }
            for service in &target.services {
                let ip = service
                    .host
                    .parse::<std::net::IpAddr>()
                    .map_err(|_| "invalid Cube service probe address")?;
                if ip.is_unspecified()
                    || ip.is_multicast()
                    || !valid_id(&service.id)
                    || !valid_id(&service.interface_id)
                    || service.port == 0
                {
                    return Err("invalid Cube service probe".into());
                }
            }
        }
        let inner = self.inner();
        let mut observation = inner
            .observations
            .lock()
            .map_err(|_| "Cube observation state unavailable")?;
        observation.targets.retain(|id, _| active.contains(id));
        observation.cache.retain(|id, _| active.contains(id));
        let mut result = vec![];
        for target in targets {
            if observation
                .targets
                .get(&target.id)
                .is_some_and(|(previous, _)| previous != target)
            {
                observation.cache.remove(&target.id);
            }
            observation
                .targets
                .insert(target.id.clone(), (target.clone(), Instant::now()));
            if let Some(cache) = observation.cache.get(&target.id) {
                result.extend(
                    cache
                        .iter()
                        .filter(|o| o.generation == target.generation)
                        .cloned(),
                );
            }
        }
        drop(observation);
        if !inner.observing.swap(true, Ordering::AcqRel) {
            let weak = Arc::downgrade(inner);
            if std::thread::Builder::new()
                .name("cvm-guest-observer".into())
                .spawn(move || {
                    while let Some(inner) = weak.upgrade() {
                        if inner.stopping.load(Ordering::Acquire) {
                            return;
                        }
                        let started = Instant::now();
                        if let Err(error) = inner.observe_cycle() {
                            eprintln!("CVM Guest observations: {error}");
                        }
                        drop(inner);
                        std::thread::sleep(
                            Duration::from_secs(15).saturating_sub(started.elapsed()),
                        );
                    }
                })
                .is_err()
            {
                inner.observing.store(false, Ordering::Release);
                return Err("cannot start Cube observation worker".into());
            }
        }
        Ok(result)
    }
}

impl Inner {
    fn observe_cycle(&self) -> Result<()> {
        let targets = self
            .observations
            .lock()
            .map_err(|_| "Cube observation state unavailable")?
            .targets
            .values()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(180))
            .map(|(target, _)| target.clone())
            .collect::<Vec<_>>();
        let cursor = AtomicUsize::new(0);
        let workers = targets.len().div_ceil(8).clamp(2, 32);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(target) = targets.get(index) else {
                        break;
                    };
                    if self.stopping.load(Ordering::Acquire) {
                        break;
                    }
                    let result = match self
                        .observe_one(target)
                        .or_else(|_| empty_observation(target, "Guest 观测暂不可用"))
                    {
                        Ok(result) => result,
                        Err(error) => {
                            eprintln!("CVM observation timestamp unavailable: {error}");
                            continue;
                        }
                    };
                    match self.observations.lock() {
                        Ok(mut observations) => {
                            if observations
                                .targets
                                .get(&target.id)
                                .is_some_and(|(current, _)| current == target)
                            {
                                let cache =
                                    observations.cache.entry(target.id.clone()).or_default();
                                cache.push_back(result);
                                while cache.len() > 4 {
                                    cache.pop_front();
                                }
                            }
                        }
                        Err(_) => {
                            eprintln!("CVM observation cache unavailable");
                            return;
                        }
                    }
                });
            }
        });
        Ok(())
    }

    fn observe_one(&self, target: &ObservationTarget) -> Result<InstanceObservation> {
        let mut out = empty_observation(target, "")?;
        let observed = self.state()?.records.get(&target.id).and_then(binding);
        let Some(observed) = observed else {
            out.runtime_state = if matches!(target.power, PowerState::Stopped | PowerState::Deleted)
            {
                ObservedRuntimeState::Stopped
            } else {
                ObservedRuntimeState::Missing
            };
            return Ok(out);
        };
        out.runtime_id = Some(observed.runtime.clone());
        let response = self.cube_timeout(
            "List",
            json!({"id":observed.runtime}),
            Duration::from_secs(2),
        )?;
        let boxes = crate::array(&response, "items")
            .iter()
            .filter(|b| {
                crate::text(b, "id") == observed.runtime
                    && crate::text(&b["labels"], "brocade.cvm.instance") == target.id
                    && crate::text(&b["labels"], "brocade.cvm.spec_sha256") == observed.launch
            })
            .collect::<Vec<_>>();
        if boxes.len() != 1 {
            out.runtime_state = ObservedRuntimeState::Missing;
            return Ok(out);
        }
        if !running(boxes[0]) {
            out.runtime_state = ObservedRuntimeState::Stopped;
            return Ok(out);
        }
        out.runtime_state = ObservedRuntimeState::Running;
        match self.guest_metrics(&observed.runtime, &observed.interfaces) {
            Ok(metrics) => {
                out.guest_state = Reachability::Reachable;
                out.observed_at_unix_millis = metrics.observed_at_unix_millis;
                out.metrics = Some(metrics);
            }
            Err(_) => {
                out.guest_state = Reachability::Unreachable;
                out.error = Some("Guest 采样通道无响应".into());
            }
        }
        if self
            .state()?
            .records
            .get(&target.id)
            .and_then(binding)
            .as_ref()
            != Some(&observed)
        {
            out.metrics = None;
            out.runtime_state = ObservedRuntimeState::Transitioning;
            out.guest_state = Reachability::Unknown;
            return Ok(out);
        }
        let address = self
            .access_address_with_timeout(&observed.runtime, Duration::from_millis(500))
            .ok();
        if let Some(address) = &address {
            out.ssh_state = probe(address, 22).0;
        }
        for service in &target.services {
            let allowed = observed.interfaces.iter().any(|nic| {
                nic.id == service.interface_id
                    && (nic
                        .addresses
                        .iter()
                        .any(|a| a.split_once('/').is_some_and(|(ip, _)| ip == service.host))
                        || (nic.device == "eth0" && address.as_ref() == Some(&service.host)))
            });
            let (state, latency_millis) = if allowed {
                probe(&service.host, service.port)
            } else {
                (Reachability::Unknown, None)
            };
            out.services.push(ServiceProbeResult {
                id: service.id.clone(),
                state,
                latency_millis,
            });
        }
        Ok(out)
    }
}

fn empty_observation(target: &ObservationTarget, error: &str) -> Result<InstanceObservation> {
    Ok(InstanceObservation {
        id: target.id.clone(),
        generation: target.generation,
        observed_at_unix_millis: crate::now_millis()?,
        runtime_id: None,
        runtime_state: ObservedRuntimeState::Unknown,
        guest_state: Reachability::Unknown,
        ssh_state: Reachability::Unknown,
        metrics: None,
        error: if error.is_empty() {
            None
        } else {
            Some(error.into())
        },
        services: vec![],
    })
}

fn probe(host: &str, port: u16) -> (Reachability, Option<u32>) {
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        return (Reachability::Unknown, None);
    };
    let started = Instant::now();
    match std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::new(ip, port),
        Duration::from_millis(250),
    ) {
        Ok(_) => (
            Reachability::Reachable,
            Some(started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32),
        ),
        Err(_) => (Reachability::Unreachable, None),
    }
}
