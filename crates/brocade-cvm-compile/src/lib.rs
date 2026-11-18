//! Pure InstanceSpec -> pinned Cube-native artifacts. No host I/O or environment lookup.
use brocade_core::hash::sha256_hex;
use brocade_deployment::cvm::*;
use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::OnceLock;

pub mod entry_network;
pub mod host_network;

pub type Result<T> = std::result::Result<T, String>;
pub fn canonical<T: Serialize>(value: &T) -> Result<String> {
    let mut value = serde_json::to_value(value).map_err(|_| "CVM artifact serialization failed")?;
    value.sort_all_objects();
    serde_json::to_string_pretty(&value)
        .map(|s| s + "\n")
        .map_err(|_| "CVM artifact encoding failed".into())
}
pub fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(sha256_hex(canonical(value)?.as_bytes()))
}

/// Pure stable identities, not public IP or NIC allocations. A collision is rejected by the
/// node-wide planner before publication, never resolved by changing a frozen product.
pub fn chain_binding(instance: &str, chain: &str) -> ChainEgressBinding {
    let hash = sha256_hex(format!("{CHAIN_EGRESS_CONTRACT}:{instance}:{chain}").as_bytes());
    let word = u32::from_str_radix(&hash[..8], 16).expect("SHA-256 hex word");
    ChainEgressBinding {
        chain_id: chain.into(),
        mark: 0xb000_0000 | (word & 0x0fff_ffff),
        device: format!("bcg{}", &hash[..12]),
    }
}

pub fn chain_bindings(instance: &str, routing: &ContainerRoutes) -> Vec<ChainEgressBinding> {
    routing
        .chain_ids()
        .into_iter()
        .map(|chain| chain_binding(instance, chain))
        .collect()
}

/// Compile the same subscriber client transport used by end-to-end authorization probes.
/// There is no direct outbound, auto-routing or host-DNS takeover in this product.
pub fn compile_chain_client(
    instance: &str,
    tenant: &str,
    user: &str,
    ingress_node: &str,
    target: &brocade_deployment::protocol::E2eProbeTarget,
) -> Result<ChainClientArtifact> {
    if !valid_id(instance)
        || !valid_id(user)
        || !valid_id(&target.chain_id)
        || !valid_id(&target.ingress_id)
        || target.port == 0
        || target.dial_host.is_empty()
        || target.dial_host.len() > 253
        || target.uuid.is_empty()
        || target.uuid.len() > 1024
        || tenant.is_empty()
        || tenant.len() > 96
    {
        return Err("invalid subscriber chain client input".into());
    }
    let binding = chain_binding(instance, &target.chain_id);
    let render = |target: &brocade_deployment::protocol::E2eProbeTarget| -> Result<String> {
        let base = brocade_deployment::client_proxy::client_config(target, 0, "none");
        let mut config: Value =
            serde_json::from_str(&base).map_err(|_| "chain client rendering failed")?;
        config["inbounds"] = json!([{
            "tag":"chain-egress", "port":0, "protocol":"tun",
            "settings":{"name":binding.device,"MTU":CHAIN_STACK_MTU},
            "sniffing":{"enabled":true,"destOverride":["http","tls","quic"],"routeOnly":false}
        }]);
        config["log"] = json!({"loglevel":"info","access":"none"});
        config["routing"] = json!({"domainStrategy":"AsIs","rules":[{
            "type":"field","inboundTag":["chain-egress"],"network":"tcp,udp","outboundTag":"probe-out"
        }]});
        canonical(&config)
    };
    let content = render(target)?;
    let mut safe = target.clone();
    safe.uuid = "[redacted]".into();
    if let brocade_deployment::protocol::E2eProbeSecurity::Hysteria2(h) = &mut safe.security {
        if h.salamander_password.is_some() {
            h.salamander_password = Some("[redacted]".into());
        }
    }
    let redacted = render(&safe)?;
    let mut artifact = ChainClientArtifact {
        blocked: false,
        instance_id: instance.into(),
        chain_id: target.chain_id.clone(),
        tenant_id: tenant.into(),
        user_id: user.into(),
        app_id: target
            .app_id
            .clone()
            .ok_or("subscriber chain application missing")?,
        ingress_id: target.ingress_id.clone(),
        ingress_node: ingress_node.into(),
        binding,
        sha256: sha256_hex(content.as_bytes()),
        content,
        redacted,
        guard: String::new(),
    };
    artifact.guard = host_network::chain_guard(&artifact, false);
    Ok(artifact)
}

pub fn normalize_image(manifest: &CvmImageManifest) -> Result<Value> {
    if manifest.content.len() > MAX_IMAGE_MANIFEST_BYTES
        || !valid_digest(&manifest.sha256)
        || sha256_hex(manifest.content.as_bytes()) != manifest.sha256
    {
        return Err("CVM image manifest digest or size invalid".into());
    }
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        DescriptorPool::decode(include_bytes!(concat!(env!("OUT_DIR"), "/cube.bin")).as_ref())
            .expect("build validates Cube descriptors")
    });
    let descriptor = pool
        .get_message_by_name("cubelet.services.cubebox.v1.RunCubeSandboxRequest")
        .ok_or("Cube descriptor missing")?;
    let raw: Value =
        serde_json::from_str(&manifest.content).map_err(|_| "CVM image JSON invalid")?;
    let message = DynamicMessage::deserialize(descriptor, raw)
        .map_err(|_| "CVM image protobuf JSON invalid")?;
    let mut bytes = Vec::new();
    message
        .serialize_with_options(
            &mut serde_json::Serializer::new(&mut bytes),
            &SerializeOptions::new()
                .use_proto_field_name(true)
                .use_enum_numbers(true)
                .stringify_64_bit_integers(false)
                .skip_default_fields(false),
        )
        .map_err(|_| "CVM image normalization failed")?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "CVM normalized image invalid")?;
    validate_image(&value)?;
    Ok(value)
}
fn validate_image(value: &Value) -> Result<()> {
    let containers = value["containers"]
        .as_array()
        .ok_or("Cube image containers missing")?;
    if containers.len() != 1 || !containers[0]["resources"].is_null() {
        return Err("Cube image must have one capacity-independent environment".into());
    }
    for volume in value["volumes"]
        .as_array()
        .ok_or("Cube image volumes missing")?
    {
        if volume["volume_source"]["empty_dir"]["SizeLimit"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        {
            return Err("Cube image has fixed disk capacity".into());
        }
    }
    for annotations in [
        &value["annotations"],
        &containers[0]["annotations"],
        &containers[0]["image"]["annotations"],
    ] {
        if annotations.as_object().is_some_and(|m| {
            m.keys().any(|key| {
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
            return Err("capacity-bound image snapshots unsupported".into());
        }
    }
    root_name(value)?;
    Ok(())
}
fn root_name(value: &Value) -> Result<&str> {
    value["containers"][0]["volume_mounts"]
        .as_array()
        .and_then(|mounts| mounts.iter().find(|m| m["container_path"] == "/"))
        .and_then(|m| m["name"].as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Cube image root mount missing".into())
}
pub fn primary_network_policy(routing: &ContainerRoutes) -> Value {
    let mut denied = routing
        .routes
        .iter()
        .filter(|r| r.destination.is_ipv4())
        .map(|r| r.destination.to_string())
        .collect::<Vec<_>>();
    denied.sort();
    denied.dedup();
    // A verified primary selector handles the complete mixed route order when chains are in use.
    // It rejects NIC-selected packets and terminally forwards chain-selected packets before NAT.
    if !routing.chain_ids().is_empty() {
        denied.clear();
    }
    json!({"allow_internet_access":true,"allow_out":[],"deny_out":denied,"rules":[]})
}
/// Exact legacy renderer retained for shadow verification; local runtime facts bind later.
pub fn sandbox_request(
    instance: &str,
    spec: &InstanceSpec,
    normalized: &Value,
    backend: &str,
) -> Result<Value> {
    let resources = &spec.resources;
    let mut request = normalized.clone();
    request["requestID"] = json!(format!("brocade-create-{instance}"));
    request["namespace"] = json!("default");
    request["backend"] = json!(backend);
    request["exposed_ports"] = json!([]);
    request["cube_network_config"] = if spec.routing.chain_ids().is_empty() {
        primary_network_policy(&spec.routing)
    } else {
        json!({"allow_internet_access":false,"allow_out":[],"deny_out":["0.0.0.0/0"],"rules":[]})
    };
    request["annotations"]["cube.master.desired.sandbox.id"] = json!(instance);
    request["labels"] = json!({"brocade.cvm.instance":instance});
    request["containers"][0]["resources"] = json!({"cpu":format!("{}m",u64::from(resources.vcpus)*1000),"mem":format!("{}Mi",resources.memory_bytes/(1024*1024))});
    let root = root_name(&request)?.to_owned();
    let mut found = false;
    for volume in request["volumes"]
        .as_array_mut()
        .ok_or("Cube image volumes missing")?
    {
        if volume["name"] == root && volume["volume_source"]["empty_dir"].is_object() {
            volume["volume_source"]["empty_dir"]["SizeLimit"] =
                json!(resources.root_disk_bytes.to_string());
            found = true;
        }
    }
    if !found {
        return Err("capacity-independent writable root missing".into());
    }
    Ok(request)
}
pub fn compile(input: &CvmArtifactInput) -> Result<CvmArtifactBundle> {
    if !valid_id(&input.instance_id) || !input.spec.execution.is_cube() {
        return Err("Cube compiler requires a Cube instance".into());
    }
    let image = input.spec.image.as_ref().ok_or("Cube image missing")?;
    if image.sha256 != input.image_manifest.sha256 || image.boot_mode != "cube-oci" {
        return Err("frozen Cube image does not match the pinned manifest".into());
    }
    input
        .capabilities
        .resource_limits
        .as_ref()
        .ok_or("Cube resource limits missing")?
        .validate_resources(&input.spec.resources)?;
    if input.spec.interfaces.len() > MAX_INTERFACES
        || input.spec.interfaces.len() > usize::from(input.capabilities.max_interfaces)
        || !valid_public_key(&input.spec.ssh_public_key)
    {
        return Err("Cube interfaces or SSH public key invalid".into());
    }
    let backend = input
        .capabilities
        .cube_storage_backend
        .as_deref()
        .ok_or("Cube storage backend missing")?;
    if !matches!(backend, "plain" | "xfs" | "s3")
        || backend == "s3" && input.capabilities.stable_storage
    {
        return Err("Cube storage contract invalid".into());
    }
    input.capabilities.validate()?;
    input.capabilities.validate_interfaces(
        &input
            .spec
            .interfaces
            .iter()
            // Managed business NICs are Store allocations from publication IDs, not
            // entries in the host's local contact-network catalogue.
            .filter(|nic| nic.allocated.is_none())
            .map(|nic| nic.connection.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut ids = std::collections::BTreeSet::new();
    let mut devices = std::collections::BTreeMap::new();
    for nic in &input.spec.interfaces {
        if !valid_id(&nic.id) || !ids.insert(&nic.id) {
            return Err("duplicate or invalid Cube interface".into());
        }
        if nic.connection.purpose == InterfacePurpose::Business {
            if !input.capabilities.managed_networks || !input.capabilities.configure_network {
                return Err("host does not support managed business networks".into());
            }
            let allocation = nic
                .allocated
                .as_ref()
                .ok_or("business NIC has no Store allocation")?;
            if nic.connection.source_id != allocation.publication_id
                || !valid_id(&allocation.publication_id)
            {
                return Err("business NIC does not match its Store publication".into());
            }
            devices.insert(nic.id.clone(), allocation.clone());
        } else if nic.allocated.is_some() {
            return Err("contact NIC cannot carry business allocation".into());
        }
    }
    input.spec.routing.validate(&devices)?;
    if !input.spec.routing.chain_ids().is_empty() && !input.capabilities.chain_egress {
        return Err("host has not verified primary-NIC chain egress".into());
    }
    let normalized = normalize_image(&input.image_manifest)?;
    let mut sandbox = sandbox_request(&input.instance_id, &input.spec, &normalized, backend)?;
    // The policy is an independently hot-applied product; omit it from boot identity.
    sandbox
        .as_object_mut()
        .ok_or("Cube request is not an object")?
        .remove("cube_network_config");
    if input.capabilities.stable_storage {
        sandbox["annotations"]["brocade.cvm.protected_root"] = json!("true");
    }
    // Never inherit host-bound annotation values from an image template.
    for key in [
        "brocade.cvm.expected_root",
        "cube.master.components.cube-shim.version",
    ] {
        if let Some(map) = sandbox["annotations"].as_object_mut() {
            map.remove(key);
        }
    }
    let mut routing =
        serde_json::to_value(&input.spec.routing).map_err(|_| "Cube routing invalid")?;
    routing
        .as_object_mut()
        .ok_or("Cube routing is not an object")?
        .remove("revision");
    let mut attachments = json!({"contract":"cubevs-attachments-v2","instance_id":input.instance_id,"interfaces":input.spec.interfaces,"routing":routing});
    let bindings = chain_bindings(&input.instance_id, &input.spec.routing);
    if !bindings.is_empty() {
        attachments["chain_egress"] = json!(bindings);
    }
    let guest = json!({"contract":"brocade-guest-init-v1","user":"cube","ssh_public_key":input.spec.ssh_public_key,"regenerate_host_keys_once":true});
    let mut files = Vec::new();
    for (kind, value) in [
        (CvmArtifactKind::Sandbox, sandbox),
        (
            CvmArtifactKind::Egress,
            primary_network_policy(&input.spec.routing),
        ),
        (CvmArtifactKind::Attachments, attachments),
        (CvmArtifactKind::GuestInit, guest),
    ] {
        let content = canonical(&value)?;
        if content.len() > 4 * 1024 * 1024 {
            return Err("CVM artifact content exceeds limits".into());
        }
        files.push(CvmArtifact {
            kind,
            sha256: sha256_hex(content.as_bytes()),
            content,
        });
    }
    let bundle = CvmArtifactBundle {
        compiler: CVM_ARTIFACT_COMPILER.into(),
        input_sha256: digest(input)?,
        spec_sha256: sha256_hex(
            &serde_json::to_vec(&input.spec).map_err(|_| "Cube spec encoding failed")?,
        ),
        input: input.clone(),
        files,
    };
    if serde_json::to_vec(&bundle)
        .map_err(|_| "CVM bundle encoding failed")?
        .len()
        > 11 * 1024 * 1024
    {
        return Err("CVM artifact bundle exceeds transport limits".into());
    }
    Ok(bundle)
}
/// Recompute every product and the input commitment before any privileged use.
pub fn verify(bundle: &CvmArtifactBundle, desired: &InstanceDesired) -> Result<()> {
    if bundle.input.instance_id != desired.id
        || bundle.input.spec != desired.spec
        || bundle.spec_sha256 != desired.spec_sha256
        || compile(&bundle.input)? != *bundle
    {
        return Err("CVM artifact bundle is not the compiled frozen input".into());
    }
    Ok(())
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct LocalBindings {
    pub runtime_id: Option<String>,
    pub expected_root: Option<String>,
    pub shim_version: Option<String>,
}
pub fn bind(
    bundle: &CvmArtifactBundle,
    kind: CvmArtifactKind,
    desired: &InstanceDesired,
    local: &LocalBindings,
) -> Result<Value> {
    let artifact = bundle.file(kind)?;
    if sha256_hex(artifact.content.as_bytes()) != artifact.sha256 {
        return Err("CVM artifact digest invalid".into());
    }
    let mut value: Value =
        serde_json::from_str(&artifact.content).map_err(|_| "CVM artifact JSON invalid")?;
    match kind {
        CvmArtifactKind::Sandbox => {
            // Derived labels/policy are not arbitrary node overrides.
            value["labels"]["brocade.cvm.spec_sha256"] = json!(desired.spec_sha256);
            value["labels"]["brocade.cvm.sandbox_sha256"] = json!(artifact.sha256);
            value["cube_network_config"] = if desired.spec.routing.chain_ids().is_empty() {
                serde_json::from_str(&bundle.file(CvmArtifactKind::Egress)?.content)
                    .map_err(|_| "CVM policy JSON invalid")?
            } else {
                json!({"allow_internet_access":false,"allow_out":[],"deny_out":["0.0.0.0/0"],"rules":[]})
            };
            for (key, binding) in [
                ("brocade.cvm.expected_root", &local.expected_root),
                (
                    "cube.master.components.cube-shim.version",
                    &local.shim_version,
                ),
            ] {
                if let Some(binding) = binding {
                    if !valid_local(binding) {
                        return Err("invalid Cube local binding".into());
                    }
                    value["annotations"][key] = json!(binding);
                }
            }
        }
        CvmArtifactKind::Attachments => {
            let runtime = local.runtime_id.as_deref().unwrap_or(&desired.id);
            if !valid_local(runtime) {
                return Err("invalid Cube runtime binding".into());
            }
            value["runtime_id"] = json!(runtime);
            value["generation"] = json!(desired.generation);
            value["routing"]["revision"] = json!(desired.spec.routing.revision);
            value["detach"] = json!(desired.detached_interfaces);
            value["release"] = json!(false);
            value["forget"] = json!(false);
        }
        CvmArtifactKind::Egress | CvmArtifactKind::GuestInit => {}
    }
    Ok(value)
}
fn valid_local(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
/// Fixed executor: declarations cannot supply shell text, users, paths or arbitrary commands.
pub const INIT_SCRIPT: &str = r#"set -eu; id cube >/dev/null; install -d -m 0700 -o cube -g cube /home/cube/.ssh; printf '%s\n' "$1" > /home/cube/.ssh/authorized_keys; chown cube:cube /home/cube/.ssh/authorized_keys; chmod 0600 /home/cube/.ssh/authorized_keys; install -d -m 0700 /root/.ssh; if [ ! -e /root/.ssh/brocade-initialized ]; then rm -f /etc/ssh/ssh_host_*; ssh-keygen -A; touch /root/.ssh/brocade-initialized; if [ -s /run/sshd.pid ]; then kill -HUP "$(cat /run/sshd.pid)"; fi; fi"#;
pub fn guest_key(value: &Value) -> Result<&str> {
    if value["contract"] != "brocade-guest-init-v1"
        || value["user"] != "cube"
        || value["regenerate_host_keys_once"] != true
    {
        return Err("unsupported Guest init contract".into());
    }
    value["ssh_public_key"]
        .as_str()
        .filter(|key| valid_public_key(key))
        .ok_or_else(|| "invalid Guest initialization key".into())
}
