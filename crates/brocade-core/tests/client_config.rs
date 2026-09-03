mod fixture;

use brocade_core::{
    client_config::SubscriptionClientConfig,
    compile::compile,
    model::{
        Action, ExternalOutbound, ExternalOutboundProtocol, ExternalOutboundSecurity,
        ExternalWarpBinding, Hysteria2, HysteriaMasquerade, HysteriaObfs, IngressWires,
        ModelSnapshot, Projection, ProjectionEndpoint, RealityXhttp, Transport, Xhttp, XhttpXmux,
    },
};
use fixture::demo_snapshot;

fn set_projection_host(snapshot: &mut ModelSnapshot, host: &str) {
    snapshot.apps[0].ingresses[0].projection = Projection {
        v4: Some(ProjectionEndpoint {
            host: host.to_owned(),
            port: 443,
            download: None,
        }),
        v6: None,
    };
}

fn projection_host(snapshot: &ModelSnapshot) -> &str {
    &snapshot.apps[0].ingresses[0]
        .projection
        .v4
        .as_ref()
        .unwrap()
        .host
}

fn assert_topology_change_gates_projection(
    mut topology: ModelSnapshot,
    change: impl FnOnce(&mut ModelSnapshot),
) {
    set_projection_host(&mut topology, "old.edge.example");
    let ingress_id = topology.apps[0].ingresses[0].id.clone();
    let initial = SubscriptionClientConfig::from_snapshot(&topology);
    let mut desired = topology.clone();
    change(&mut desired);
    set_projection_host(&mut desired, "new.edge.example");

    let next = SubscriptionClientConfig::advance(Some(&initial), &desired);
    assert_eq!(
        next.pending_topology(&topology),
        vec![format!("ingress:{ingress_id}:projection")]
    );
    assert_eq!(
        projection_host(&next.apply(topology).unwrap()),
        "old.edge.example"
    );
    assert_eq!(
        projection_host(&next.apply(desired).unwrap()),
        "new.edge.example"
    );
}

fn assert_topology_change_allows_projection(
    mut topology: ModelSnapshot,
    change: impl FnOnce(&mut ModelSnapshot),
) {
    set_projection_host(&mut topology, "old.edge.example");
    let initial = SubscriptionClientConfig::from_snapshot(&topology);
    let mut desired = topology.clone();
    change(&mut desired);
    set_projection_host(&mut desired, "new.edge.example");

    let next = SubscriptionClientConfig::advance(Some(&initial), &desired);
    assert!(next.pending_topology(&topology).is_empty());
    assert_eq!(
        projection_host(&next.apply(topology).unwrap()),
        "new.edge.example"
    );
}

fn hysteria2_topology() -> ModelSnapshot {
    let mut topology = demo_snapshot();
    let node_id = topology.apps[0].ingresses[0].node.clone();
    topology
        .nodes
        .iter_mut()
        .find(|node| node.id == node_id)
        .unwrap()
        .certificate_name = Some("old-cert.example".to_owned());
    topology.apps[0].ingresses[0].wires = IngressWires::Hysteria2(Hysteria2::default());
    topology
}

fn xhttp_topology() -> ModelSnapshot {
    let mut topology = demo_snapshot();
    let ingress = &mut topology.apps[0].ingresses[0];
    let reality = ingress.wires.reality().unwrap().clone();
    ingress.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality,
        xhttp: Xhttp {
            path: "/checkpoint".to_owned(),
            host: Some("old.upload.example".to_owned()),
            xmux: Some(XhttpXmux::with_concurrency(8)),
            tuning: None,
            mode: Default::default(),
        },
    }));
    topology
}

fn warp_topology() -> ModelSnapshot {
    let mut topology = demo_snapshot();
    let app = &mut topology.apps[0];
    let (step_index, rule_index) = app
        .steps
        .iter()
        .enumerate()
        .find_map(|(step_index, step)| {
            step.rules
                .iter()
                .position(|rule| matches!(rule.action, Action::Egress { .. }))
                .map(|rule_index| (step_index, rule_index))
        })
        .expect("demo topology has a terminal egress rule");
    let node_id = app.steps[step_index].node.clone();
    let chain_id = app.steps[step_index].chain.clone();
    let tenant = app
        .chains
        .iter()
        .find(|chain| chain.id == chain_id)
        .unwrap()
        .tenant
        .clone();
    app.steps[step_index].rules[rule_index].action = Action::Proxy {
        outbound: "warp-checkpoint".to_owned(),
    };
    topology.external_outbounds.push(ExternalOutbound {
        id: "warp-checkpoint".to_owned(),
        tenant,
        name: "WARP checkpoint fixture".to_owned(),
        address: "engage.cloudflareclient.com".to_owned(),
        port: 2408,
        protocol: ExternalOutboundProtocol::Warp {
            mtu: 1280,
            keep_alive: 25,
            allowed_ips: vec!["0.0.0.0/0".to_owned(), "::/0".to_owned()],
            no_kernel_tun: false,
            domain_strategy: "ForceIP".to_owned(),
            workers: 0,
        },
        security: ExternalOutboundSecurity::None,
        bindings: vec![ExternalWarpBinding {
            node: node_id,
            device_id: "device-checkpoint".to_owned(),
            account_id: "account-checkpoint".to_owned(),
            registered_at: "2026-09-02T00:00:00.000Z".to_owned(),
            endpoint_address: None,
            endpoint_port: None,
            mtu: None,
            keep_alive: None,
            allowed_ips: None,
            no_kernel_tun: None,
            domain_strategy: None,
            workers: None,
            private_key: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_owned(),
            peer_public_key: "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=".to_owned(),
            local_addresses: vec!["172.16.0.2/32".to_owned()],
            reserved: vec![0, 0, 0],
        }],
    });
    topology
}

#[test]
fn chain_name_advances_without_changing_any_node_plan() {
    let topology = demo_snapshot();
    let initial = SubscriptionClientConfig::from_snapshot(&topology);
    let mut desired = topology.clone();
    desired.revision += 1;
    desired.apps[0].chains[0].name = "新的客户端名称".to_owned();
    desired.apps[0].chains[0].subscription_country = Some("TW".to_owned());

    let next = SubscriptionClientConfig::advance(Some(&initial), &desired);
    let composed = next.apply(topology.clone()).unwrap();
    assert_eq!(composed.apps[0].chains[0].name, "新的客户端名称");
    assert_eq!(
        composed.apps[0].chains[0].subscription_country.as_deref(),
        Some("TW")
    );

    let before = compile(&topology);
    let after = compile(&composed);
    for node in &topology.nodes {
        assert_eq!(
            before.project_node(&node.id).unwrap(),
            after.project_node(&node.id).unwrap(),
            "client-only naming must not alter node {}",
            node.id
        );
    }
}

#[test]
fn deleting_desired_chain_keeps_last_client_name_until_topology_removes_it() {
    let topology = demo_snapshot();
    let mut renamed = topology.clone();
    let chain_id = renamed.apps[0].chains[0].id.clone();
    renamed.apps[0].chains[0].name = "已经生效的名字".to_owned();
    let renamed_client = SubscriptionClientConfig::from_snapshot(&renamed);

    let mut deleted = renamed;
    deleted.apps[0].chains.retain(|chain| chain.id != chain_id);
    let after_delete = SubscriptionClientConfig::advance(Some(&renamed_client), &deleted);
    let composed = after_delete.apply(topology).unwrap();
    assert_eq!(
        composed.apps[0]
            .chains
            .iter()
            .find(|chain| chain.id == chain_id)
            .unwrap()
            .name,
        "已经生效的名字"
    );
}

#[test]
fn projection_candidate_switches_only_when_topology_contract_matches() {
    let mut topology = demo_snapshot();
    let ingress_id = topology.apps[0].ingresses[0].id.clone();
    topology.apps[0].ingresses[0].projection = Projection {
        v4: Some(ProjectionEndpoint {
            host: "old.edge.example".to_owned(),
            port: 443,
            download: None,
        }),
        v6: None,
    };
    let initial = SubscriptionClientConfig::from_snapshot(&topology);

    let mut desired = topology.clone();
    desired.apps[0].ingresses[0].port += 1;
    desired.apps[0].ingresses[0]
        .projection
        .v4
        .as_mut()
        .unwrap()
        .host = "new.edge.example".to_owned();
    let next = SubscriptionClientConfig::advance(Some(&initial), &desired);
    assert_eq!(
        next.pending_topology(&topology),
        vec![format!("ingress:{ingress_id}:projection")]
    );

    let old_composed = next.apply(topology).unwrap();
    let old_ingress = old_composed.apps[0]
        .ingresses
        .iter()
        .find(|ingress| ingress.id == ingress_id)
        .unwrap();
    assert_eq!(
        old_ingress.projection.v4.as_ref().unwrap().host,
        "old.edge.example"
    );

    let new_composed = next.apply(desired).unwrap();
    assert!(next.pending_topology(&new_composed).is_empty());
    let new_ingress = new_composed.apps[0]
        .ingresses
        .iter()
        .find(|ingress| ingress.id == ingress_id)
        .unwrap();
    assert_eq!(
        new_ingress.projection.v4.as_ref().unwrap().host,
        "new.edge.example"
    );
}

#[test]
fn public_projection_change_activates_without_a_topology_change() {
    let mut topology = demo_snapshot();
    topology.apps[0].ingresses[0].projection = Projection {
        v4: Some(ProjectionEndpoint {
            host: "old.edge.example".to_owned(),
            port: 443,
            download: None,
        }),
        v6: None,
    };
    let initial = SubscriptionClientConfig::from_snapshot(&topology);
    let mut desired = topology.clone();
    desired.apps[0].ingresses[0]
        .projection
        .v4
        .as_mut()
        .unwrap()
        .host = "new.edge.example".to_owned();

    let next = SubscriptionClientConfig::advance(Some(&initial), &desired);
    assert!(next.pending_topology(&topology).is_empty());
    let composed = next.apply(topology).unwrap();
    assert_eq!(
        composed.apps[0].ingresses[0]
            .projection
            .v4
            .as_ref()
            .unwrap()
            .host,
        "new.edge.example"
    );
}

#[test]
fn reality_identity_change_holds_projection_until_topology_matches() {
    assert_topology_change_gates_projection(demo_snapshot(), |desired| {
        desired.apps[0].ingresses[0].identity.public_key = "new-public-key".to_owned();
    });
    assert_topology_change_gates_projection(demo_snapshot(), |desired| {
        desired.apps[0].ingresses[0].identity.short_ids[0] = "0123456789abcdef".to_owned();
    });
}

#[test]
fn hysteria_handshake_change_holds_projection_until_topology_matches() {
    assert_topology_change_gates_projection(hysteria2_topology(), |desired| {
        desired.apps[0].ingresses[0]
            .wires
            .hysteria2_mut()
            .unwrap()
            .obfs = Some(HysteriaObfs::Salamander {
            password: "new-mask".to_owned(),
        });
    });
    assert_topology_change_gates_projection(hysteria2_topology(), |desired| {
        desired
            .nodes
            .iter_mut()
            .find(|node| node.certificate_name.as_deref() == Some("old-cert.example"))
            .unwrap()
            .certificate_name = Some("new-cert.example".to_owned());
    });
}

#[test]
fn hysteria_client_profile_change_holds_projection_until_topology_matches() {
    assert_topology_change_gates_projection(hysteria2_topology(), |desired| {
        desired.apps[0].ingresses[0]
            .wires
            .hysteria2_mut()
            .unwrap()
            .bandwidth
            .up = Some("200 mbps".to_owned());
    });
    assert_topology_change_gates_projection(hysteria2_topology(), |desired| {
        desired.apps[0].ingresses[0]
            .wires
            .hysteria2_mut()
            .unwrap()
            .quic
            .max_stream_receive_window = Some(65_536);
    });
}

#[test]
fn server_only_hysteria_change_does_not_hold_projection() {
    assert_topology_change_allows_projection(hysteria2_topology(), |desired| {
        let hysteria = desired.apps[0].ingresses[0].wires.hysteria2_mut().unwrap();
        hysteria.masquerade = HysteriaMasquerade::Proxy {
            url: "https://cover.example".to_owned(),
        };
        hysteria.quic.max_incoming_streams = Some(128);
    });
}

#[test]
fn flow_change_holds_projection_until_topology_matches() {
    assert_topology_change_gates_projection(demo_snapshot(), |desired| {
        desired.apps[0].ingresses[0]
            .wires
            .set_flow(Some(String::new()));
    });
}

#[test]
fn ingress_client_transport_controls_advance_without_topology_release() {
    let topology = xhttp_topology();
    let initial = SubscriptionClientConfig::from_snapshot(&topology);
    let mut desired = topology.clone();
    let ingress = &mut desired.apps[0].ingresses[0];
    ingress.wires.reality_mut().unwrap().fingerprint = "firefox".to_owned();
    let xhttp = ingress.wires.xhttp_mut().unwrap();
    xhttp.host = Some("new.upload.example".to_owned());
    xhttp.xmux = Some(XhttpXmux {
        max_concurrency: None,
        max_connections: Some(4),
        h_max_request_times: XhttpXmux::DEFAULT_REQUEST_TIMES,
        h_max_reusable_secs: XhttpXmux::DEFAULT_REUSABLE_SECS,
        h_keep_alive_period_secs: Some(15),
    });

    let next = SubscriptionClientConfig::advance(Some(&initial), &desired);
    assert!(next.pending_topology(&topology).is_empty());

    let composed = next.apply(topology).unwrap();
    let ingress = &composed.apps[0].ingresses[0];
    assert_eq!(ingress.wires.reality().unwrap().fingerprint, "firefox");
    let xhttp = ingress.wires.xhttp().unwrap();
    assert_eq!(xhttp.host.as_deref(), Some("new.upload.example"));
    assert_eq!(
        xhttp.xmux,
        desired.apps[0].ingresses[0].wires.xhttp().unwrap().xmux
    );
}

#[test]
fn composed_client_config_restores_serving_warp_bindings() {
    let topology = warp_topology();
    compile(&topology).ensure_publishable().unwrap();
    let initial = SubscriptionClientConfig::from_snapshot(&topology);
    assert!(
        initial.external_outbounds[0].bindings.is_empty(),
        "the client checkpoint must not persist WARP device identities"
    );

    let composed = initial.apply(topology.clone()).unwrap();
    assert_eq!(
        composed.external_outbounds[0].bindings,
        topology.external_outbounds[0].bindings
    );
    compile(&composed).ensure_publishable().unwrap();

    let mut renamed = topology.clone();
    renamed.apps[0].chains[0].name = "Renamed through WARP".to_owned();
    let next = SubscriptionClientConfig::advance(Some(&initial), &renamed);
    let composed = next.apply(topology).unwrap();
    assert_eq!(composed.apps[0].chains[0].name, "Renamed through WARP");
    compile(&composed).ensure_publishable().unwrap();
}

#[test]
fn projection_tombstone_disables_the_serving_projection_without_deleting_topology() {
    let mut topology = demo_snapshot();
    topology.apps[0].ingresses[0].projection = Projection {
        v4: Some(ProjectionEndpoint {
            host: "old.edge.example".to_owned(),
            port: 443,
            download: None,
        }),
        v6: None,
    };
    let initial = SubscriptionClientConfig::from_snapshot(&topology);
    let mut desired = topology.clone();
    desired.apps[0].ingresses[0].projection = Projection::default();

    let next = SubscriptionClientConfig::advance(Some(&initial), &desired);
    let composed = next.apply(topology).unwrap();
    assert_eq!(
        composed.apps[0].ingresses[0].projection,
        Projection::default()
    );
}

#[test]
fn deleting_a_desired_chain_does_not_reorder_the_still_serving_chain() {
    let mut topology = demo_snapshot();
    let mut middle = topology.apps[0].chains[0].clone();
    middle.id = "temporarily-deleted".to_owned();
    middle.name = "Still serving".to_owned();
    topology.apps[0].chains.insert(1, middle);
    let initial = SubscriptionClientConfig::from_snapshot(&topology);
    let mut desired = topology.clone();
    desired.apps[0]
        .chains
        .retain(|chain| chain.id != "temporarily-deleted");

    let next = SubscriptionClientConfig::advance(Some(&initial), &desired);
    let composed = next.apply(topology).unwrap();
    assert_eq!(
        composed.apps[0]
            .chains
            .iter()
            .map(|chain| chain.id.as_str())
            .collect::<Vec<_>>(),
        initial.chain_order[&composed.apps[0].id]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
}

#[test]
fn unrelated_revision_keeps_the_same_client_config() {
    let topology = demo_snapshot();
    let first = SubscriptionClientConfig::from_snapshot(&topology);
    let mut unrelated = topology;
    unrelated.revision += 1;
    unrelated.settings.probe.timeout_secs += 1;
    let second = SubscriptionClientConfig::advance(Some(&first), &unrelated);
    assert_eq!(first, second);
}
