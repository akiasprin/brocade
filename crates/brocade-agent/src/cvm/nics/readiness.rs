//! Process-local evidence that the current active transport finished applying.
//! Persisted intent alone must never enable a new host OUTPUT selector.
use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct Applied {
    bindings: BTreeMap<(String, bool), String>,
}
static APPLIED: OnceLock<Mutex<Applied>> = OnceLock::new();

fn key(binding: &NicBinding) -> (String, bool) {
    (binding.id.clone(), binding.side == BindingSide::Provider)
}
fn fingerprint(binding: &NicBinding) -> Result<String> {
    let mut binding = binding.clone();
    // Local metering may tighten this flag without changing the published intent.
    // Its fence is enforced by the NIC worker, independently of host activation.
    binding.nic.quota_exceeded = false;
    Ok(brocade_core::hash::sha256_hex(
        &serde_json::to_vec(&binding).map_err(|_| "invalid native transport readiness identity")?,
    ))
}
impl Applied {
    fn begin(&mut self, binding: &NicBinding, desired: &NicDesired) {
        self.bindings.retain(|key, _| {
            desired
                .bindings
                .iter()
                .any(|current| &self::key(current) == key)
        });
        self.bindings.remove(&key(binding));
    }
    fn observe(&mut self, binding: &NicBinding, observation: &LinkObservation) -> Result<()> {
        let key = key(binding);
        self.bindings.remove(&key);
        if binding.phase == BindingPhase::Active
            && observation.configured
            && (observation.reachable == Some(true) || binding.nic.is_blocked())
        {
            if self.bindings.len() >= NODE_BINDINGS * 4 {
                return Err("native transport readiness capacity exceeded".into());
            }
            self.bindings.insert(key, fingerprint(binding)?);
        }
        Ok(())
    }
    fn contains(&self, binding: &NicBinding) -> Result<bool> {
        Ok(self.bindings.get(&key(binding)) == Some(&fingerprint(binding)?))
    }
    fn ready(
        &self,
        instance: &str,
        interfaces: &[InterfaceSpec],
        snapshot: &NicDesired,
    ) -> Result<bool> {
        for interface in interfaces {
            let Some(assigned) = &interface.allocated else {
                continue;
            };
            if assigned.is_blocked() {
                continue;
            }
            let Some(consumer) = snapshot.bindings.iter().find(|binding| {
                binding.side == BindingSide::Consumer
                    && binding.phase == BindingPhase::Active
                    && binding.instance_id == instance
                    && binding.interface_id == interface.id
                    && binding.consumer_node == snapshot.node_id
                    && same_allocation(&binding.nic, assigned)
            }) else {
                return Ok(false);
            };
            if !self.contains(consumer)? {
                return Ok(false);
            }
            // A same-machine provider also has to install its Active forwarding
            // policy. A successful consumer apply cannot stand in for that side.
            if consumer.provider_node == snapshot.node_id {
                let Some(provider) = snapshot.bindings.iter().find(|binding| {
                    binding.side == BindingSide::Provider
                        && binding.id == consumer.id
                        && binding.phase == BindingPhase::Active
                }) else {
                    return Ok(false);
                };
                if !self.contains(provider)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}
fn same_allocation(left: &AllocatedNic, right: &AllocatedNic) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.blocked = false;
    right.blocked = false;
    left.quota_exceeded = false;
    right.quota_exceeded = false;
    left == right
}
fn state() -> Result<std::sync::MutexGuard<'static, Applied>> {
    APPLIED
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "native transport readiness unavailable".into())
}
pub(in crate::cvm) struct Activation {
    _applied: std::sync::MutexGuard<'static, Applied>,
}
pub(super) fn apply(
    binding: &NicBinding,
    desired: &NicDesired,
    execute: impl FnOnce() -> Result<LinkObservation>,
) -> Result<LinkObservation> {
    let mut applied = state()?;
    applied.begin(binding, desired);
    let observation = execute()?;
    applied.observe(binding, &observation)?;
    Ok(observation)
}
pub(super) fn forget(binding: &NicBinding) -> Result<()> {
    state()?.bindings.remove(&key(binding));
    Ok(())
}
#[cfg(test)]
pub(in crate::cvm) fn observe(binding: &NicBinding, observation: &LinkObservation) -> Result<()> {
    state()?.observe(binding, observation)
}
pub(in crate::cvm) fn activation(
    desired: &InstanceDesired,
    snapshot: &NicDesired,
) -> Result<Option<Activation>> {
    let applied = state()?;
    if desired.power != PowerState::Deleted
        && !applied.ready(&desired.id, &desired.spec.interfaces, snapshot)?
    {
        return Ok(None);
    }
    // Keep evidence valid until the host selector commit finishes. The transport
    // worker invalidates it under this same lock before changing kernel state.
    Ok(Some(Activation { _applied: applied }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Vec<InterfaceSpec>, NicDesired) {
        let bindings = super::super::tests::scheduling_bindings(2);
        let interfaces = vec![InterfaceSpec {
            id: bindings[0].interface_id.clone(),
            allocated: Some(bindings[0].nic.clone()),
            connection: InterfaceRequest {
                source_id: bindings[0].nic.publication_id.clone(),
                purpose: InterfacePurpose::Business,
                default_ipv4: false,
                default_ipv6: false,
            },
        }];
        (
            interfaces,
            NicDesired {
                node_id: "node-sweep".into(),
                bindings,
                ..Default::default()
            },
        )
    }
    fn connected() -> LinkObservation {
        LinkObservation {
            configured: true,
            reachable: Some(true),
            ..Default::default()
        }
    }
    #[test]
    fn activation_requires_both_local_sides_to_complete_the_active_generation() {
        let (interfaces, mut desired) = fixture();
        let mut applied = Applied::default();
        let ready = |applied: &Applied, desired: &NicDesired| {
            applied.ready("cvm-sweep-0", &interfaces, desired).unwrap()
        };
        assert!(!ready(&applied, &desired), "intent is not an apply receipt");
        for binding in &mut desired.bindings {
            binding.phase = BindingPhase::Preparing;
            applied.observe(binding, &connected()).unwrap();
        }
        assert!(!ready(&applied, &desired));
        for binding in &mut desired.bindings {
            binding.phase = BindingPhase::Active;
            binding.generation += 1;
        }
        assert!(!ready(&applied, &desired));
        applied.observe(&desired.bindings[0], &connected()).unwrap();
        assert!(
            !ready(&applied, &desired),
            "the provider is not configured yet"
        );
        applied.observe(&desired.bindings[1], &connected()).unwrap();
        assert!(ready(&applied, &desired));
        desired.bindings[0].generation += 1;
        assert!(
            !ready(&applied, &desired),
            "old receipts cannot activate new intent"
        );
        applied.observe(&desired.bindings[0], &connected()).unwrap();
        assert!(ready(&applied, &desired));
        applied.begin(&desired.bindings[0], &desired);
        assert!(
            !ready(&applied, &desired),
            "an interrupted apply must not remain ready"
        );
        applied
            .observe(&desired.bindings[0], &LinkObservation::default())
            .unwrap();
        assert!(!ready(&applied, &desired));
        applied.observe(&desired.bindings[0], &connected()).unwrap();
        assert!(ready(&applied, &desired));
        applied
            .observe(
                &desired.bindings[0],
                &LinkObservation {
                    configured: true,
                    reachable: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            !ready(&applied, &desired),
            "configuration alone is not connectivity"
        );
        applied.observe(&desired.bindings[0], &connected()).unwrap();
        assert!(
            !ready(&Applied::default(), &desired),
            "restart requires fresh apply evidence"
        );
    }
    #[test]
    fn readiness_matches_allocation_and_policy_but_preserves_explicit_fences() {
        let (mut interfaces, mut desired) = fixture();
        let mut applied = Applied::default();
        for binding in &desired.bindings {
            applied.observe(binding, &connected()).unwrap();
        }
        assert!(applied.ready("cvm-sweep-0", &interfaces, &desired).unwrap());
        desired.bindings[0].nic.quota_exceeded = true;
        assert!(applied.ready("cvm-sweep-0", &interfaces, &desired).unwrap());
        desired.bindings[0].routing.revision += 1;
        assert!(!applied.ready("cvm-sweep-0", &interfaces, &desired).unwrap());
        applied.observe(&desired.bindings[0], &connected()).unwrap();
        interfaces[0].allocated.as_mut().unwrap().slot += 1;
        assert!(!applied.ready("cvm-sweep-0", &interfaces, &desired).unwrap());
        interfaces[0].allocated.as_mut().unwrap().blocked = true;
        assert!(Applied::default()
            .ready("cvm-sweep-0", &interfaces, &desired)
            .unwrap());
        applied.begin(&desired.bindings[0], &NicDesired::default());
        assert!(
            applied.bindings.is_empty(),
            "removed bindings do not accumulate"
        );
    }
}
