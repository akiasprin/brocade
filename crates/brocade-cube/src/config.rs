use crate::{files, rpc, Result};
use brocade_core::hash::sha256_hex;
use brocade_deployment::cvm::{
    Capabilities, ContainerRoutes, Image, InstanceDesired, NetworkKind, NetworkSource,
    ResourceLimits,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The primary IPv4 NIC supplies host NAT only outside explicit business routes.
/// Enforcing their prefixes in CubeVS also prevents binding the primary source
/// address from bypassing a selected business NIC or its failure policy.
pub(crate) fn primary_network_policy(routing: &ContainerRoutes) -> Value {
    let mut denied = routing
        .routes
        .iter()
        .filter(|route| route.destination.is_ipv4())
        .map(|route| route.destination.to_string())
        .collect::<Vec<_>>();
    denied.sort();
    denied.dedup();
    json!({"allow_internet_access":true,"allow_out":[],"deny_out":denied,"rules":[]})
}

#[derive(Clone, Deserialize)]
pub(crate) struct Config {
    #[serde(default)]
    virtualization_mode: VirtualizationMode,
    pub(crate) cubelet_socket: PathBuf,
    pub(crate) network_inspection_url: String,
    pub(crate) storage_backend: String,
    pub(crate) capabilities: CapabilityConfig,
    pub(crate) image_files: BTreeMap<String, PathBuf>,
}

/// Administrator choices; executor-derived capabilities are not configuration.
#[derive(Clone, Deserialize)]
pub(crate) struct CapabilityConfig {
    create: bool,
    stop: bool,
    start: bool,
    delete: bool,
    pub(crate) stable_storage: bool,
    retain_on_delete: bool,
    restart: bool,
    reinstall: bool,
    configure: bool,
    hot_attach: bool,
    hot_detach: bool,
    pub(crate) max_interfaces: u16,
    allocatable_vcpus: u32,
    allocatable_memory_bytes: u64,
    allocatable_disk_bytes: u64,
    pub(crate) images: Vec<Image>,
    resource_limits: Option<ResourceLimits>,
    pub(crate) network_sources: Vec<NetworkSource>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum VirtualizationMode {
    #[default]
    Kvm,
    Pvm,
}

impl Config {
    pub(crate) fn load(path: &Path) -> Result<(Self, BTreeMap<String, Value>)> {
        if !files::clean_absolute(path) {
            return Err("Cube configuration path must be absolute and clean".into());
        }
        let mut cfg: Self =
            serde_json::from_slice(&files::checked_file(path, 2 * 1024 * 1024, true)?)
                .map_err(|_| "invalid private Cube configuration")?;
        if !matches!(cfg.storage_backend.as_str(), "xfs" | "plain" | "s3")
            || !files::clean_absolute(&cfg.cubelet_socket)
        {
            return Err("Cube requires an explicit CoW backend and absolute Unix socket".into());
        }
        let url = reqwest::Url::parse(&cfg.network_inspection_url)
            .map_err(|_| "invalid Cube inspection URL")?;
        let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
        if url.scheme() != "http"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/v1/network/taps"
            || !host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        {
            return Err(
                "Cube network inspection must use the literal loopback taps endpoint".into(),
            );
        }
        if !(1..=3).contains(&cfg.capabilities.max_interfaces)
            || (cfg.capabilities.stable_storage && cfg.storage_backend == "s3")
        {
            return Err("invalid Cube interface capacity or protected storage backend".into());
        }
        let mut public = 0;
        for source in &mut cfg.capabilities.network_sources {
            source.max_attachments = Some(1);
            if source.kind == NetworkKind::Public {
                source.supports_ipv6 = Some(false);
                public += 1;
            }
        }
        if public != 1 {
            return Err("Cube requires exactly one primary network source".into());
        }
        cfg.configured_capabilities().validate()?;
        let mut images = BTreeMap::new();
        for image in &cfg.capabilities.images {
            if image.architecture != std::env::consts::ARCH || image.boot_mode != "cube-oci" {
                return Err(
                    "Cube system image architecture or boot mode does not match this host".into(),
                );
            }
            let path = cfg
                .image_files
                .get(&image.id)
                .ok_or("Cube image file binding missing")?;
            if !files::clean_absolute(path) {
                return Err("Cube image path must be absolute".into());
            }
            let bytes = files::checked_file(path, 2 * 1024 * 1024, false)?;
            if sha256_hex(&bytes) != image.sha256 {
                return Err("Cube image manifest digest does not match".into());
            }
            let raw =
                serde_json::from_slice(&bytes).map_err(|_| "invalid Cube system manifest JSON")?;
            let message: rpc::cb::RunCubeSandboxRequest =
                rpc::decode("cubelet.services.cubebox.v1.RunCubeSandboxRequest", raw)?;
            let normalized =
                rpc::encode("cubelet.services.cubebox.v1.RunCubeSandboxRequest", message)?;
            validate_image(&normalized)?;
            images.insert(image.id.clone(), normalized);
        }
        Ok((cfg, images))
    }

    pub(crate) fn configured_capabilities(&self) -> Capabilities {
        let cfg = &self.capabilities;
        Capabilities {
            network_quota: None,
            managed_networks: false,
            backend: match self.virtualization_mode {
                VirtualizationMode::Kvm => "cube-kvm-v1",
                VirtualizationMode::Pvm => "cube-pvm-v1",
            }
            .into(),
            guest_terminal: true,
            guest_terminal_persistent: true,
            configure_network: true,
            route_services: false,
            primary_ip_delivery: false,
            create: cfg.create,
            stop: cfg.stop,
            start: cfg.start,
            delete: cfg.delete,
            stable_storage: cfg.stable_storage,
            retain_on_delete: cfg.retain_on_delete,
            restart: cfg.restart,
            reinstall: cfg.reinstall,
            configure: cfg.configure,
            hot_attach: cfg.hot_attach,
            hot_detach: cfg.hot_detach,
            max_interfaces: cfg.max_interfaces,
            allocatable_vcpus: cfg.allocatable_vcpus,
            allocatable_memory_bytes: cfg.allocatable_memory_bytes,
            allocatable_disk_bytes: cfg.allocatable_disk_bytes,
            images: cfg.images.clone(),
            resource_limits: cfg.resource_limits.clone(),
            network_sources: cfg.network_sources.clone(),
        }
    }

    pub(crate) fn create_request(
        &self,
        images: &BTreeMap<String, Value>,
        desired: &InstanceDesired,
    ) -> Result<Value> {
        let spec = &desired.spec;
        let image = serde_json::to_value(&spec.image).map_err(|_| "invalid frozen system image")?;
        if !self
            .capabilities
            .images
            .iter()
            .any(|i| serde_json::to_value(i).ok().as_ref() == Some(&image))
        {
            return Err("frozen system image is no longer in the verified catalogue".into());
        }
        let resources = &spec.resources;
        self.capabilities
            .resource_limits
            .as_ref()
            .ok_or("independent Cube resources unavailable")?
            .validate_resources(resources)?;
        let mut request = images
            .get(&spec.image.id)
            .ok_or("Cube system image unavailable")?
            .clone();
        request["requestID"] = json!(format!("brocade-create-{}", desired.id));
        request["namespace"] = json!("default");
        request["backend"] = json!(self.storage_backend);
        request["exposed_ports"] = json!([]);
        request["cube_network_config"] = primary_network_policy(&spec.routing);
        request["annotations"]["cube.master.desired.sandbox.id"] = json!(desired.id);
        request["labels"] = json!({"brocade.cvm.instance":desired.id,"brocade.cvm.spec_sha256":desired.spec_sha256});
        request["containers"][0]["resources"] = json!({"cpu":format!("{}m", u64::from(resources.vcpus)*1000),"mem":format!("{}Mi",resources.memory_bytes/(1024*1024))});
        let root = root_name(&request)?.to_owned();
        let mut found = false;
        for volume in request["volumes"]
            .as_array_mut()
            .ok_or("Cube system image volumes missing")?
        {
            if crate::text(volume, "name") == root
                && volume["volume_source"]["empty_dir"].is_object()
            {
                volume["volume_source"]["empty_dir"]["SizeLimit"] =
                    json!(resources.root_disk_bytes.to_string());
                found = true;
            }
        }
        if !found {
            return Err("system image lacks a capacity-independent writable root mount".into());
        }
        Ok(request)
    }
}

pub(crate) fn root_name(value: &Value) -> Result<&str> {
    crate::array(&value["containers"][0], "volume_mounts")
        .iter()
        .find(|m| crate::text(m, "container_path") == "/")
        .map(|m| crate::text(m, "name"))
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "system image has no explicit root mount".into())
}

fn validate_image(value: &Value) -> Result<()> {
    if crate::array(value, "containers").len() != 1
        || !value["containers"][0]["resources"].is_null()
    {
        return Err(
            "system image manifests must contain one environment and no CPU or memory capacity"
                .into(),
        );
    }
    for volume in crate::array(value, "volumes") {
        if !crate::text(&volume["volume_source"]["empty_dir"], "SizeLimit").is_empty() {
            return Err("system image manifests must not contain disk capacity".into());
        }
    }
    for annotations in [
        &value["annotations"],
        &value["containers"][0]["annotations"],
        &value["containers"][0]["image"]["annotations"],
    ] {
        if annotations.as_object().is_some_and(|map| {
            map.keys().any(|key| {
                [
                    "appsnapshot",
                    "template.spec_fingerprint",
                    "system_disk_size",
                    "writable_layer_size",
                ]
                .iter()
                .any(|word| key.contains(word))
            })
        }) {
            return Err("capacity-bound memory snapshots cannot be used as system images".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod primary_policy_tests {
    use super::*;

    #[test]
    fn primary_nat_is_available_without_business_routes() {
        let policy = primary_network_policy(&ContainerRoutes::default());
        assert_eq!(policy["allow_internet_access"], true);
        assert_eq!(policy["deny_out"], json!([]));
    }

    #[test]
    fn selected_business_prefixes_cannot_fall_back_through_primary_nat() {
        let routes: ContainerRoutes = serde_json::from_value(json!({
            "revision":1,"priorities":[],"dns":[],
            "routes":[
                {"destination":"203.0.113.0/24","interface_id":"nic-b"},
                {"destination":"0.0.0.0/0","interface_id":"nic-a"},
                {"destination":"203.0.113.0/24","interface_id":"nic-a"},
                {"destination":"::/0","interface_id":"nic-b"}
            ]
        }))
        .unwrap();
        let policy = primary_network_policy(&routes);
        assert_eq!(policy["allow_internet_access"], true);
        assert_eq!(policy["deny_out"], json!(["0.0.0.0/0", "203.0.113.0/24"]));
        assert_eq!(
            primary_network_policy(&ContainerRoutes::default())["deny_out"],
            json!([])
        );
    }
}
