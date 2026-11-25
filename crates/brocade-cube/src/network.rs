use crate::{backend::Inner, records::Record, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use brocade_deployment::cvm::*;
use serde_json::{json, Value};

const CONTRACT: &str = "cubevs-attachments-v2";
const RESULT_KEY: &str = "cube.network.attachments.result";

impl Inner {
    pub(crate) fn network_rpc(
        &self,
        action: &str,
        runtime: &str,
        payload: Option<Value>,
    ) -> Result<Value> {
        let mut annotations = json!({"cube.network.attachments.action":action});
        if let Some(payload) = payload {
            let value =
                serde_json::to_string(&payload).map_err(|_| "invalid Cube network request")?;
            if value.len() > 128 * 1024 {
                return Err("Cube network request exceeds limit".into());
            }
            annotations["cube.network.attachments.request"] = json!(value);
        }
        let request = json!({"requestID":format!("brocade-network-{action}"),"sandboxID":runtime,"annotations":annotations});
        let timeout = if action == "inventory" {
            std::time::Duration::from_secs(10)
        } else {
            std::time::Duration::from_secs(240)
        };
        let response = self.runtime.block_on(async {
            tokio::time::timeout(timeout, async {
                // Cubelet has one mutation lock for attachments and quotas. Keep
                // both workers in the same queue; waiting consumes the RPC budget.
                // Read-only capability checks need not wait for Guest convergence.
                let _guard = if matches!(action, "prepare" | "reconcile" | "quota" | "guard_routes")
                {
                    Some(self.network_mutation.lock().await)
                } else {
                    None
                };
                self.rpc.cube("Update", request).await
            })
            .await
            .map_err(|_| "Cube network RPC deadline exceeded")?
        })?;
        network_result(action, &response)
    }

    pub(crate) fn native_capabilities(&self) -> Result<Capabilities> {
        let catalogue = self.network_rpc("inventory", "", None)?;
        let max = crate::number(&catalogue, "max_interfaces");
        if !(1..=3).contains(&max) {
            return Err("unsupported Cube network catalogue".into());
        }
        let flag = |key: &str| {
            catalogue[key]
                .as_bool()
                .ok_or_else(|| format!("Cube network capability missing: {key}"))
        };
        let mut caps = self.cfg.configured_capabilities();
        if flag("network_quota")? {
            let quota = self.network_rpc("quota_inventory", "", Some(json!({})))?;
            caps.network_quota = Some(
                serde_json::from_value(quota["capacity"].clone())
                    .map_err(|_| "invalid Cube network quota capacity")?,
            );
        }
        self.quota_supported.store(
            caps.network_quota.is_some(),
            std::sync::atomic::Ordering::Release,
        );
        caps.managed_networks = flag("managed_nics")?;
        caps.primary_ip_delivery = flag("primary_ip_delivery")?;
        caps.route_services = flag("route_services")?;
        caps.chain_egress = flag("chain_egress")?;
        caps.max_interfaces = caps.max_interfaces.min(max as u16);
        Ok(caps)
    }

    pub(crate) fn validate_nics<'a>(
        &self,
        desired: &'a InstanceDesired,
    ) -> Result<&'a InterfaceSpec> {
        let nics = &desired.spec.interfaces;
        if nics.is_empty()
            || nics.len() > usize::from(self.cfg.capabilities.max_interfaces)
            || nics.len() > MAX_INTERFACES
        {
            return Err("invalid Cube interface count".into());
        }
        let public = self
            .cfg
            .capabilities
            .network_sources
            .first()
            .ok_or("Cube primary source missing")?;
        let mut contact = None;
        let mut ids = std::collections::BTreeSet::new();
        for nic in nics {
            if !valid_id(&nic.id) || !valid_id(&nic.connection.source_id) || !ids.insert(&nic.id) {
                return Err("invalid, duplicated or repeated-source Cube interface".into());
            }
            if nic.connection.purpose == InterfacePurpose::Contact {
                if nic.connection.source_id != public.id
                    || contact.is_some()
                    || nic.connection.default_ipv4
                    || nic.connection.default_ipv6
                {
                    return Err(
                        "Cube requires its single original independent contact interface".into(),
                    );
                }
                contact = Some(nic);
            } else if nic.allocated.is_none() {
                return Err("business NIC requires a Store allocation".into());
            }
        }
        contact.ok_or_else(|| "original Cube contact interface required".into())
    }

    pub(crate) fn network_desired(
        &self,
        record: &Record,
        desired: &InstanceDesired,
    ) -> Result<Value> {
        self.artifact_value(
            desired,
            CvmArtifactKind::Attachments,
            &brocade_cvm_compile::LocalBindings {
                runtime_id: Some(if record.runtime.is_empty() {
                    record.instance.clone()
                } else {
                    record.runtime.clone()
                }),
                ..Default::default()
            },
        )
    }

    pub(crate) fn prepare_nics(
        &self,
        record: &mut Record,
        desired: &InstanceDesired,
    ) -> Result<()> {
        let request = if desired.power == PowerState::Running {
            self.network_desired(record, desired)?
        } else {
            // Stopped instances retain prospective leases without consuming a creation product.
            self.cleanup_network_desired(record, desired)
        };
        let evidence = self.network_rpc(
            "prepare",
            crate::text(&request, "runtime_id"),
            Some(request.clone()),
        )?;
        let shim = crate::text(&evidence, "shim_version");
        if !valid_id(shim) {
            return Err("Cube preparation lacks a pinned shim version".into());
        }
        record.native_shim_version = shim.into();
        Ok(())
    }

    pub(crate) fn ensure_nics(
        &self,
        record: &mut Record,
        desired: &InstanceDesired,
    ) -> Result<(Vec<InterfaceObservation>, NetworkObservation, Vec<String>)> {
        // Fence new chain-selected flows before relaxing the original NAT policy.
        // On removal the guard RPC still installs the remaining NIC selectors first.
        let request = self.network_desired(record, desired)?;
        self.network_rpc(
            "guard_routes",
            crate::text(&request, "runtime_id"),
            Some(request.clone()),
        )?;
        let primary_policy =
            self.artifact_value(desired, CvmArtifactKind::Egress, &Default::default())?;
        let policy = self.cube(
            "Update",
            json!({
                "requestID":format!("brocade-primary-network-{}",record.instance),
                "sandboxID":record.runtime,
                "annotations":{"cube.master.update_action":"network"},
                "cube_network_config":primary_policy
            }),
        )?;
        crate::successful(&policy)?;
        let request = self.network_desired(record, desired)?;
        let evidence = self.network_rpc(
            "reconcile",
            crate::text(&request, "runtime_id"),
            Some(request.clone()),
        )?;
        let interfaces: Vec<InterfaceObservation> =
            serde_json::from_value(evidence["interfaces"].clone())
                .map_err(|_| "Cube interface evidence invalid")?;
        let network: NetworkObservation = serde_json::from_value(evidence["network"].clone())
            .map_err(|_| "Cube routing evidence invalid")?;
        let detached = serde_json::from_value(evidence["detached_interfaces"].clone())
            .map_err(|_| "Cube detached interface evidence invalid")?;
        if interfaces.len() != desired.spec.interfaces.len() {
            return Err("Cube network evidence incomplete".into());
        }
        record.actual_vcpus = crate::number(&evidence, "actual_vcpus");
        record.actual_memory_bytes = crate::number(&evidence, "actual_memory_bytes");
        record.save(&self.root)?;
        Ok((interfaces, network, detached))
    }

    fn cleanup_network_desired(&self, record: &Record, desired: &InstanceDesired) -> Value {
        let mut request = json!({"contract":CONTRACT,"instance_id":record.instance,
            "runtime_id":if record.runtime.is_empty(){&record.instance}else{&record.runtime},
            "generation":desired.generation,"interfaces":desired.spec.interfaces,"routing":desired.spec.routing,
            "detach":desired.detached_interfaces});
        let bindings = brocade_cvm_compile::chain_bindings(&desired.id, &desired.spec.routing);
        if !bindings.is_empty() {
            request["chain_egress"] = json!(bindings);
        }
        request
    }

    pub(crate) fn remove_nics(&self, record: &mut Record, desired: &InstanceDesired) -> Result<()> {
        let mut request = self.cleanup_network_desired(record, desired);
        request["release"] = json!(true);
        request["forget"] = json!(desired.power == PowerState::Deleted);
        let evidence = self.network_rpc(
            "reconcile",
            crate::text(&request, "runtime_id"),
            Some(request.clone()),
        )?;
        if evidence["network_absent"] != true {
            return Err("Cube native network cleanup unconfirmed".into());
        }
        let detached: Vec<String> = serde_json::from_value(evidence["detached_interfaces"].clone())
            .map_err(|_| "Cube stopped detach evidence is invalid")?;
        if desired
            .detached_interfaces
            .iter()
            .any(|id| !detached.contains(id))
        {
            return Err("Cubelet has not confirmed stopped interface cleanup; upgrade the matched Cube runtime".into());
        }
        record.detached_interfaces = detached;
        record.save(&self.root)
    }
}

fn evidence(response: &Value) -> Result<Value> {
    let bytes = STANDARD
        .decode(crate::text(&response["ext_info"], RESULT_KEY))
        .map_err(|_| "invalid Cube network response encoding")?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("Cube network response exceeds limit".into());
    }
    let result: Value =
        serde_json::from_slice(&bytes).map_err(|_| "invalid Cube network evidence")?;
    if crate::text(&result, "contract") != CONTRACT {
        return Err("Cube network contract unsupported".into());
    }
    Ok(result)
}

fn network_result(action: &str, response: &Value) -> Result<Value> {
    if crate::successful(response).is_err() {
        let reason = match crate::text(&response["ret"], "ret_msg") {
            "Cube network reconciliation is busy; retry the same desired version"
            | "Cube network reconciliation is busy"
            | "Cube network quota convergence is busy; retry same desired version" => {
                "busy; retry the same desired version"
            }
            _ => "rejected; allocations retained",
        };
        // Do not expose Cubelet's arbitrary error text: it may contain private
        // request data or host paths. The operation and return code are safe.
        return Err(format!(
            "Cube network {action} {reason} (code {})",
            crate::number(&response["ret"], "ret_code")
        ));
    }
    evidence(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(code: u64, payload: &[u8]) -> Value {
        json!({"ret":{"ret_code":code},"ext_info":{RESULT_KEY:STANDARD.encode(payload)}})
    }

    #[test]
    fn failed_requests_do_not_require_evidence_or_expose_private_errors() {
        for payload in [b"".as_slice(), b"broken JSON"] {
            let mut reply = response(130568, payload);
            reply["ret"]["ret_msg"] = json!("private path and credentials");
            assert_eq!(
                network_result("prepare", &reply).unwrap_err(),
                "Cube network prepare rejected; allocations retained (code 130568)"
            );
        }
        let mut reply = response(130568, b"");
        reply["ret"]["ret_msg"] =
            json!("Cube network reconciliation is busy; retry the same desired version");
        assert!(network_result("reconcile", &reply)
            .unwrap_err()
            .contains("busy; retry"));
    }

    #[test]
    fn only_success_requires_valid_network_evidence() {
        assert_eq!(
            network_result("reconcile", &response(200, b"")).unwrap_err(),
            "invalid Cube network evidence"
        );
        let payload = json!({"contract":CONTRACT,"network_absent":true});
        assert_eq!(
            network_result(
                "reconcile",
                &response(200, &serde_json::to_vec(&payload).unwrap())
            )
            .unwrap(),
            payload
        );
        assert!(network_result("reconcile", &response(200, br#"{"contract":"other"}"#)).is_err());
    }
}
