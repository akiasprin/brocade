use crate::{backend::Inner, records::Record, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use brocade_deployment::cvm::*;
use serde_json::{json, Value};

const CONTRACT: &str = "cubevs-attachments-v2";
const RESULT_KEY: &str = "cube.network.attachments.result";
pub(crate) const RESTART_REQUIRED: &str = "native Cube shim restart required";

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
                let _guard = if matches!(action, "prepare" | "reconcile" | "quota") {
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
        let sources = catalogue["sources"]
            .as_object()
            .ok_or("Cube network sources unavailable")?;
        if !(1..=3).contains(&max) || sources.len() > MAX_CATALOG_ITEMS {
            return Err("unsupported Cube network catalogue".into());
        }
        let mut caps = self.cfg.configured_capabilities();
        if catalogue["network_quota"] == true {
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
        caps.managed_networks = catalogue["managed_nics"] == true;
        caps.primary_ip_delivery = catalogue["primary_ip_delivery"] == true;
        caps.route_services = catalogue["route_services"] == true;
        caps.max_interfaces = caps.max_interfaces.min(max as u16);
        let labels = caps.network_sources.clone();
        caps.network_sources
            .retain(|s| s.kind == NetworkKind::Public);
        let mut ids = sources.keys().collect::<Vec<_>>();
        ids.sort();
        for id in ids {
            if !valid_id(id) {
                return Err("invalid Cube network source identity".into());
            }
            let label = labels.iter().find(|s| &s.id == id);
            caps.network_sources.push(NetworkSource {
                id: id.clone(),
                name: label.map_or_else(|| id.clone(), |s| s.name.clone()),
                kind: NetworkKind::Internal,
                description: label.map_or_else(
                    || "机器管理员配置的自有网络".into(),
                    |s| s.description.clone(),
                ),
                max_attachments: Some(1),
                supports_ipv6: Some(!crate::text(&sources[id], "ipv6_pool").is_empty()),
            });
        }
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
            .iter()
            .find(|s| s.kind == NetworkKind::Public)
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
        let value = json!({"contract":CONTRACT,"instance_id":record.instance,
            "runtime_id":if record.runtime.is_empty(){&record.instance}else{&record.runtime},
            "generation":desired.generation,"interfaces":desired.spec.interfaces,"routing":desired.spec.routing,"detach":desired.detached_interfaces,"release":false,"forget":false});
        Ok(value)
    }

    pub(crate) fn prepare_nics(
        &self,
        record: &mut Record,
        desired: &InstanceDesired,
    ) -> Result<()> {
        let request = self.network_desired(record, desired)?;
        let evidence = self.network_rpc(
            "prepare",
            crate::text(&request, "runtime_id"),
            Some(request.clone()),
        )?;
        record.native_shim_version = crate::text(&evidence, "shim_version").into();
        Ok(())
    }

    pub(crate) fn ensure_nics(
        &self,
        record: &mut Record,
        desired: &InstanceDesired,
    ) -> Result<(Vec<InterfaceObservation>, NetworkObservation, Vec<String>)> {
        let policy = self.cube(
            "Update",
            json!({
                "requestID":format!("brocade-primary-network-{}",record.instance),
                "sandboxID":record.runtime,
                "annotations":{"cube.master.update_action":"network"},
                "cube_network_config":crate::config::primary_network_policy(&desired.spec.routing)
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

    pub(crate) fn remove_nics(&self, record: &mut Record, desired: &InstanceDesired) -> Result<()> {
        let mut request = self.network_desired(record, desired)?;
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
        // Failure responses normally have no evidence. Only a validated upgrade
        // receipt may request a restart; never parse an empty failure as evidence.
        if evidence(response).is_ok_and(|result| result["restart_required"] == true) {
            return Err(RESTART_REQUIRED.into());
        }
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

    #[test]
    fn restart_requires_a_matching_failure_receipt() {
        let payload = json!({"contract":CONTRACT,"restart_required":true});
        assert_eq!(
            network_result(
                "reconcile",
                &response(130568, &serde_json::to_vec(&payload).unwrap())
            )
            .unwrap_err(),
            RESTART_REQUIRED
        );
        assert_ne!(
            network_result(
                "reconcile",
                &response(130568, br#"{"contract":"other","restart_required":true}"#)
            )
            .unwrap_err(),
            RESTART_REQUIRED
        );
    }
}
