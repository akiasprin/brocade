use brocade_cvm_compile::*;
use brocade_deployment::cvm::*;
use serde_json::{json, Value};
fn input() -> CvmArtifactInput {
    serde_json::from_str(include_str!("golden/input.json")).unwrap()
}
fn desired(input: &CvmArtifactInput) -> InstanceDesired {
    serde_json::from_value(json!({"id":input.instance_id,"operation_id":"op-test","generation":1,"spec_sha256":brocade_core::hash::sha256_hex(&serde_json::to_vec(&input.spec).unwrap()),"spec":input.spec,"power":"running","delete_data":false,"restart_epoch":0,"reinstall_epoch":0})).unwrap()
}
#[test]
fn native_products_match_reviewed_golden_bytes() {
    let bundle = compile(&input()).unwrap();
    for file in bundle.files {
        let name = serde_json::to_value(file.kind)
            .unwrap()
            .as_str()
            .unwrap()
            .replace('/', "-");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        if std::env::var_os("UPDATE_CVM_GOLDEN").is_some() {
            std::fs::write(&path, &file.content).unwrap();
        }
        assert_eq!(file.content, std::fs::read_to_string(path).unwrap());
        assert_eq!(
            file.sha256,
            brocade_core::hash::sha256_hex(file.content.as_bytes())
        );
    }
}
#[test]
fn generation_routing_revision_and_name_are_not_boot_identity() {
    let i = input();
    let a = compile(&i).unwrap();
    let mut d = desired(&i);
    d.generation += 1;
    assert_eq!(compile(&i).unwrap(), a);
    let mut j = i.clone();
    j.spec.name = "renamed".into();
    j.spec.routing.revision += 1;
    assert_eq!(a.files, compile(&j).unwrap().files);
    assert_ne!(a.input_sha256, compile(&j).unwrap().input_sha256);
}
#[test]
fn file_changes_express_apply_cost() {
    let i = input();
    let a = compile(&i).unwrap();
    let mut j = i.clone();
    j.spec.resources.vcpus = 2;
    let changed = |other: &CvmArtifactInput| {
        let b = compile(other).unwrap();
        a.files
            .iter()
            .zip(b.files)
            .filter(|(x, y)| x.sha256 != y.sha256)
            .map(|(x, _)| x.kind)
            .collect::<Vec<_>>()
    };
    assert_eq!(changed(&j), vec![CvmArtifactKind::Sandbox]);
    j = i.clone();
    j.capabilities.managed_networks = true;
    j.spec.interfaces.push(business_nic());
    j.spec.routing=serde_json::from_value(json!({"revision":2,"priorities":[{"interface_id":"nic-egress","ipv4":100,"ipv6":null}],"dns":[],"routes":[{"destination":"0.0.0.0/0","interface_id":"nic-egress"}]})).unwrap();
    assert_eq!(
        changed(&j),
        vec![CvmArtifactKind::Egress, CvmArtifactKind::Attachments]
    );
    j = i.clone();
    j.spec.login_revision += 1;
    assert_eq!(changed(&j), vec![CvmArtifactKind::GuestInit]);
    assert_eq!(CvmArtifactKind::GuestInit.apply(), "boot_identity");
    assert_eq!(CvmArtifactKind::Attachments.bindings(), &["/runtime_id"]);
}
#[test]
fn exact_manifest_commitment_and_catalogue_are_required() {
    let i = input();
    let mut bad = i.clone();
    bad.image_manifest.content.push('\n');
    assert!(compile(&bad).is_err());
    bad = i.clone();
    bad.capabilities.images.clear();
    assert!(compile(&bad).is_err());
    bad = i.clone();
    bad.capabilities.cube_storage_backend = None;
    assert!(compile(&bad).is_err());
    bad = i.clone();
    bad.spec.login_revision = 0;
    assert!(compile(&bad).is_err());
}
#[test]
fn tampered_products_and_spec_are_rejected_before_binding() {
    let i = input();
    let b = compile(&i).unwrap();
    let d = desired(&i);
    verify(&b, &d).unwrap();
    let mut tampered = b.clone();
    tampered.files[0].content.push(' ');
    assert!(verify(&tampered, &d).is_err());
    tampered = b.clone();
    tampered.input.capabilities.stable_storage = false;
    assert!(verify(&tampered, &d).is_err());
    tampered = b.clone();
    tampered.files.pop();
    assert!(verify(&tampered, &d).is_err());
    let mut d2 = d.clone();
    d2.spec.name = "different".into();
    assert!(verify(&b, &d2).is_err());
}
#[test]
fn binding_is_fixed_and_effective_digest_is_reproducible() {
    let i = input();
    let b = compile(&i).unwrap();
    let mut d = desired(&i);
    d.generation = 9;
    let local = LocalBindings {
        runtime_id: Some("runtime-a".into()),
        expected_root: Some("original-root".into()),
        shim_version: Some("v1.2.3".into()),
    };
    let req = bind(&b, CvmArtifactKind::Sandbox, &d, &local).unwrap();
    assert_eq!(
        req["annotations"]["brocade.cvm.expected_root"],
        "original-root"
    );
    assert_eq!(
        req["labels"]["brocade.cvm.sandbox_sha256"],
        b.files[0].sha256
    );
    let n = bind(&b, CvmArtifactKind::Attachments, &d, &local).unwrap();
    assert_eq!(n["generation"], 9);
    assert_eq!(n["runtime_id"], "runtime-a");
    assert_ne!(
        digest(&n).unwrap(),
        b.file(CvmArtifactKind::Attachments).unwrap().sha256
    );
    let bad = LocalBindings {
        expected_root: Some("../../etc/passwd".into()),
        ..Default::default()
    };
    assert!(bind(&b, CvmArtifactKind::Sandbox, &d, &bad).is_err());
}
#[test]
fn engine_template_does_not_override_host_bindings_or_supply_scripts() {
    let i = input();
    let b = compile(&i).unwrap();
    let req: Value = serde_json::from_str(&b.files[0].content).unwrap();
    assert!(req["annotations"]
        .get("brocade.cvm.expected_root")
        .is_none());
    assert!(req.get("cube_network_config").is_none());
    assert!(req["labels"].get("brocade.cvm.spec_sha256").is_none());
}
#[test]
fn an_existing_pinned_manifest_does_not_depend_on_the_current_catalogue_entry() {
    let input = input();
    let before = compile(&input).unwrap();
    let mut retired = input.clone();
    retired.capabilities.images[0].id = "new-catalogue-image".into();
    assert_eq!(compile(&retired).unwrap().files, before.files);
}

fn business_nic() -> InterfaceSpec {
    serde_json::from_value(json!({"id":"nic-egress","source_id":"pub-egress","purpose":"business","default_ipv4":false,"default_ipv6":false,"allocated":{"slot":1,"publication_id":"pub-egress","publication_revision":1,"provider_node":"provider","resource_domain":"lab","allocation_id":"allocation-egress","mac":"02:00:00:00:00:01","ipv4":"10.2.0.2","ipv6":null,"public_ipv4":"192.0.2.1","public_ipv6":null,"address_mode":"shared_nat","destinations":["0.0.0.0/0"],"transport":"wireguard","transport_generation":1,"mtu":1400,"blocked":false,"quota_exceeded":false,"limits":{"max_attachments":1,"upload_bps":null,"download_bps":null,"monthly_bytes":null}}})).unwrap()
}

#[test]
fn managed_business_publications_are_independent_of_the_host_contact_catalogue() {
    let mut i = input();
    i.capabilities.managed_networks = true;
    i.spec.interfaces.push(business_nic());
    assert!(!i
        .capabilities
        .network_sources
        .iter()
        .any(|n| n.id == "pub-egress"));
    let bundle = compile(&i).unwrap();
    verify(&bundle, &desired(&i)).unwrap();
    let mut bad = i.clone();
    bad.capabilities.managed_networks = false;
    assert!(compile(&bad).is_err());
    let mut bad = i.clone();
    bad.capabilities.max_interfaces = 1;
    assert!(compile(&bad).is_err());
    let mut bad = i.clone();
    bad.spec.interfaces[1].allocated = None;
    assert!(compile(&bad).is_err());
    let mut bad = i.clone();
    bad.spec.interfaces[1].connection.source_id = "other-publication".into();
    assert!(compile(&bad).is_err());
    let mut bad = i;
    bad.spec.interfaces[0].connection.source_id = "unknown-contact-source".into();
    assert!(compile(&bad).is_err());
}

#[test]
fn chain_egress_is_a_primary_selector_not_a_business_allocation() {
    let mut i = input();
    i.capabilities.chain_egress = true;
    i.spec.routing=serde_json::from_value(json!({"revision":2,"priorities":[{"chain_id":"chain-main","ipv4":100,"ipv6":100}],"dns":[],"routes":[{"destination":"0.0.0.0/0","chain_id":"chain-main"},{"destination":"::/0","chain_id":"chain-main"}]})).unwrap();
    let b = compile(&i).unwrap();
    assert_eq!(i.spec.interfaces.len(), 1);
    let attach: Value =
        serde_json::from_str(&b.file(CvmArtifactKind::Attachments).unwrap().content).unwrap();
    assert_eq!(attach["interfaces"].as_array().unwrap().len(), 1);
    assert_eq!(attach["chain_egress"][0]["chain_id"], "chain-main");
    assert_eq!(
        attach["chain_egress"][0]["mark"],
        chain_binding(&i.instance_id, "chain-main").mark
    );
    assert_eq!(b.files, compile(&i).unwrap().files);
    let hot: Value =
        serde_json::from_str(&b.file(CvmArtifactKind::Egress).unwrap().content).unwrap();
    assert!(hot["deny_out"].as_array().unwrap().is_empty());
    let request = bind(
        &b,
        CvmArtifactKind::Sandbox,
        &desired(&i),
        &LocalBindings {
            expected_root: Some("original-root".into()),
            shim_version: Some("v1.2.3".into()),
            runtime_id: None,
        },
    )
    .unwrap();
    assert_eq!(
        request["cube_network_config"]["deny_out"],
        json!(["0.0.0.0/0"]),
        "new guests are closed until the primary classifier is installed"
    );
    i.capabilities.chain_egress = false;
    assert!(compile(&i).is_err());
}
#[test]
fn subscriber_client_has_no_host_nat_outbound_and_hides_credentials_in_review() {
    let target = brocade_deployment::protocol::E2eProbeTarget {
        app_id: Some("app-main".into()),
        chain_id: "chain-main".into(),
        chain_name: "Main".into(),
        ingress_id: "ingress-main".into(),
        dial_host: "192.0.2.7".into(),
        port: 443,
        uuid: "11111111-2222-4333-8444-555555555555".into(),
        security: brocade_deployment::protocol::E2eProbeSecurity::VlessEncryption {
            encryption: "none".into(),
        },
        xhttp: None,
        expected_exit_ips: vec![],
    };
    let a = compile_chain_client("cvm-one", "tenant", "alice", "node-entry", &target).unwrap();
    let v: Value = serde_json::from_str(&a.content).unwrap();
    assert_eq!(v["inbounds"][0]["protocol"], "tun");
    assert_eq!(v["inbounds"][0]["settings"]["MTU"], 1280);
    assert_eq!(v["outbounds"].as_array().unwrap().len(), 1);
    assert_eq!(
        v["outbounds"][0]["streamSettings"]["sockopt"]["mark"],
        CHAIN_CLIENT_SOCKET_MARK
    );
    assert_eq!(
        v["outbounds"][0]["settings"]["vnext"][0]["users"][0]["id"],
        target.uuid
    );
    assert!(!a.redacted.contains(&target.uuid));
    assert!(a.redacted.contains("[redacted]"));
    assert!(!a.content.contains("freedom"));
    let files = host_network::compile(
        &NicDesired::default(),
        &EntryDesired {
            node_id: "node-entry".into(),
            ..Default::default()
        },
        std::slice::from_ref(&a),
    )
    .unwrap();
    assert!(files.iter().all(|f| !f.content.contains(&target.uuid)));
    let guard = files
        .iter()
        .find(|f| f.kind.ends_with("guard.nft"))
        .unwrap();
    assert!(guard.content.contains("oifname !="));
    assert!(guard.content.contains(" drop\n"));
    assert_eq!(guard.content, host_network::chain_guard(&a, false));
}

#[test]
fn password_initialization_compiles_only_a_versioned_secret_reference() {
    let mut input = input();
    input.spec.login_revision = 7;
    let bundle = compile(&input).unwrap();
    let file = bundle.file(CvmArtifactKind::GuestInit).unwrap();
    let value: Value = serde_json::from_str(&file.content).unwrap();
    assert_eq!(value["contract"], "brocade-guest-init-v2");
    assert_eq!(value["credential_revision"], 7);
    assert!(value.get("password").is_none() && value.get("ssh_public_key").is_none());
    assert_eq!(compile(&input).unwrap(), bundle);
    input.spec.login_revision = 0;
    assert!(compile(&input).is_err());
}

#[test]
fn password_initialization_creates_runtime_state_and_rejects_a_stale_ssh_pid() {
    assert!(PASSWORD_INIT_SCRIPT.contains("install -d -m 0755 /run/sshd"));
    assert!(PASSWORD_INIT_SCRIPT.contains("/usr/sbin/sshd -t -f \"$config\""));
    assert!(PASSWORD_INIT_SCRIPT.contains("readlink \"/proc/$pid/exe\""));
    assert!(PASSWORD_INIT_SCRIPT.contains("= /usr/sbin/sshd"));
    assert!(PASSWORD_INIT_SCRIPT.contains("kill -HUP \"$pid\""));
    assert!(PASSWORD_INIT_SCRIPT.contains("rm -f /run/sshd.pid\n  /usr/sbin/sshd"));
}
