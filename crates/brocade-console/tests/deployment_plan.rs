use std::net::{IpAddr, Ipv4Addr};

use brocade_core::model::{
    Action, AppView, Chain, DestMatch, Dns, DomainStrategy, ExternalOutbound,
    ExternalOutboundProtocol, ExternalOutboundSecurity, ExternalWarpBinding, Grant, Hysteria2,
    HysteriaPortHop, Ingress, IngressWires, ModelSnapshot, Node, Rule, Step, Transport, User,
    WireGuardKeys,
};
use brocade_deployment::plan::{
    narrow_to_kind, plan_deployment, AppliedArtifactState, AppliedGrantsState, DeploymentKind,
    DeploymentPlan, DesiredArtifact, DesiredGrants, GrantInbound, NodeAppliedState, ObservedClient,
    ObservedInbound, PlannedAction, PlannedTarget, PlannedTargetStatus,
};
use ipnet::Ipv4Net;

#[test]
fn unchanged_applied_state_is_skipped() {
    let snapshot = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app(
            "direct",
            "hk",
            "i-direct",
            8443,
            true,
            any_egress(),
        )],
    );
    let first = plan_deployment(&snapshot, &[]).unwrap();
    let applied = applied_from_plan(&first);

    let plan = plan_deployment(&snapshot, &applied).unwrap();

    assert_eq!(plan.summary.total_targets, 1);
    assert_eq!(plan.summary.changed_targets, 0);
    assert_eq!(plan.summary.skipped_targets, 1);
    assert_eq!(plan.targets[0].status, PlannedTargetStatus::Skipped);
    assert!(plan.targets[0].actions.is_empty());
}

#[test]
fn grant_only_change_stays_in_wave_zero() {
    let before = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app(
            "direct",
            "hk",
            "i-direct",
            8443,
            false,
            any_egress(),
        )],
    );
    let applied = applied_from_plan(&plan_deployment(&before, &[]).unwrap());
    let after = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app(
            "direct",
            "hk",
            "i-direct",
            8443,
            true,
            any_egress(),
        )],
    );

    let plan = plan_deployment(&after, &applied).unwrap();

    assert_eq!(plan.summary.changed_targets, 1);
    assert_eq!(plan.summary.disruptive_targets, 0);
    assert_eq!(plan.targets[0].wave, 0);
    assert_eq!(plan.targets[0].actions, vec![PlannedAction::SyncGrants]);

    let config = narrow_to_kind(plan, DeploymentKind::Config);
    assert_eq!(config.summary.changed_targets, 0);
    assert_eq!(config.targets[0].status, PlannedTargetStatus::Skipped);
    assert!(matches!(
        config.targets[0].desired.grants,
        DesiredGrants::Unmanaged { .. }
    ));
}

#[test]
fn wireguard_change_stays_in_wave_zero() {
    let before = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        Vec::new(),
    );
    let applied = applied_from_plan(&plan_deployment(&before, &[]).unwrap());
    let after = snapshot(
        vec![
            node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System),
            node("sg", "sg.example.net", [10, 66, 0, 2], Dns::System),
        ],
        Vec::new(),
    );

    let plan = plan_deployment(&after, &applied).unwrap();
    let hk = target(&plan, "hk");
    let sg = target(&plan, "sg");

    // hk has been observed already (its xray field reads disabled), leaving only the peer table to
    // change.
    assert_eq!(hk.wave, 0);
    assert_eq!(hk.actions, vec![PlannedAction::ApplyWireGuard]);
    assert!(!hk.disruptive);

    // sg is newly enrolled with an empty baseline. Besides receiving its wg config it must also be
    // told that neither xray nor phantun should be running here — the enrollment script sets xray
    // to start at boot while this model gives it no application view at all, and phantun likewise,
    // nobody taking fake TCP in this version.
    // The crux is that neither counts as destructive: nothing was ever placed on sg, disabling
    // drops no connection, so it travels in wave 0 and enrollment is still completed in one.
    assert_eq!(sg.wave, 0);
    assert_eq!(
        sg.actions,
        vec![
            PlannedAction::ApplyWireGuard,
            PlannedAction::DisablePhantun,
            PlannedAction::DisableHy2PortHop,
            PlannedAction::DisableXray
        ]
    );
    assert!(!sg.disruptive);
}

/// Against the case above: also disabling xray, but on a machine that really did run configuration
/// we placed, which claims an exclusive wave and counts as destructive.
#[test]
fn disabling_xray_that_was_running_is_disruptive_and_takes_its_own_wave() {
    let before = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app("direct", "hk", "i-hk", 8443, true, any_egress())],
    );
    let applied = applied_from_plan(&plan_deployment(&before, &[]).unwrap());
    let after = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        Vec::new(),
    );

    let plan = plan_deployment(&after, &applied).unwrap();
    let hk = target(&plan, "hk");

    assert_eq!(hk.actions, vec![PlannedAction::DisableXray]);
    assert!(hk.disruptive);
    assert_eq!(hk.wave, 1);
}

#[test]
fn xray_structure_change_uses_canary_then_batch_wave() {
    let before = snapshot(
        vec![
            node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System),
            node("sg", "sg.example.net", [10, 66, 0, 2], Dns::System),
        ],
        vec![
            direct_app("direct-hk", "hk", "i-hk", 8443, true, any_egress()),
            direct_app("direct-sg", "sg", "i-sg", 9443, true, any_egress()),
        ],
    );
    let applied = applied_from_plan(&plan_deployment(&before, &[]).unwrap());
    let after = snapshot(
        vec![
            node(
                "hk",
                "hk.example.net",
                [10, 66, 0, 1],
                Dns::Servers(vec!["8.8.8.8".to_owned()]),
            ),
            node(
                "sg",
                "sg.example.net",
                [10, 66, 0, 2],
                Dns::Servers(vec!["8.8.8.8".to_owned()]),
            ),
        ],
        vec![
            direct_app("direct-hk", "hk", "i-hk", 8443, true, any_egress()),
            direct_app("direct-sg", "sg", "i-sg", 9443, true, any_egress()),
        ],
    );

    let plan = plan_deployment(&after, &applied).unwrap();
    let hk = target(&plan, "hk");
    let sg = target(&plan, "sg");

    assert_eq!(plan.summary.disruptive_targets, 2);
    assert_eq!(hk.wave, 1);
    assert_eq!(hk.actions, vec![PlannedAction::ApplyXray]);
    assert_eq!(sg.wave, 2);
    assert_eq!(sg.actions, vec![PlannedAction::ApplyXray]);
}

#[test]
fn present_xray_to_disabled_gets_dedicated_wave() {
    let before = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app(
            "direct",
            "hk",
            "i-direct",
            8443,
            true,
            any_egress(),
        )],
    );
    let applied = applied_from_plan(&plan_deployment(&before, &[]).unwrap());
    let after = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        Vec::new(),
    );

    let plan = plan_deployment(&after, &applied).unwrap();

    assert_eq!(plan.summary.changed_targets, 1);
    assert_eq!(plan.summary.disruptive_targets, 1);
    assert_eq!(plan.targets[0].wave, 1);
    assert_eq!(plan.targets[0].actions, vec![PlannedAction::DisableXray]);
}

/// "The previous deployment did not manage this machine's grants" must count as not knowing rather
/// than as nothing to do.
///
/// The difference has real consequences: a machine skipped once that reported unmanaged never has
/// its grant state updated again — every grant added afterwards (new users, the probe credential)
/// is permanently unable to reach it, while the release page says there is nothing to do. Hit on
/// preview, where the symptom was an added user having no effect and no diagnosable cause.
#[test]
fn unmanaged_grants_are_treated_as_unknown_not_as_converged() {
    let snapshot = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app("direct", "hk", "i-hk", 8443, true, any_egress())],
    );
    let first = plan_deployment(&snapshot, &[]).unwrap();

    // Not one byte of the artifacts changed and only the grants did — and this machine's grant
    // state is unmanaged.
    let applied = vec![NodeAppliedState {
        hy2_port_hop: AppliedArtifactState::Unmanaged,
        running_xray: None,
        phantun: AppliedArtifactState::Unmanaged,
        node_id: "hk".to_owned(),
        wireguard: applied_artifact(&first.targets[0].desired.wireguard),
        xray: applied_artifact(&first.targets[0].desired.xray),
        grants: AppliedGrantsState::Unmanaged,
    }];

    let plan = plan_deployment(&snapshot, &applied).unwrap();
    assert_eq!(
        plan.targets[0].actions,
        vec![PlannedAction::SyncGrants],
        "unmanaged 意味着不知道机器上是什么样，该同步一次"
    );
    // Synchronizing grants restarts no xray and drops no connection, so the extra work this round
    // costs almost nothing.
    assert!(!plan.targets[0].disruptive);
}

/// The convergence test is the set of `(email, uuid, flow)`. With the people added correctly on the
/// machine it should judge there is nothing to do, even though the observation carries no level —
/// that is an argument to `adu` and `inbounduser` cannot read it back.
#[test]
fn grants_converge_when_observation_carries_email_uuid_and_flow() {
    let snapshot = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app("direct", "hk", "i-hk", 8443, true, any_egress())],
    );
    let first = plan_deployment(&snapshot, &[]).unwrap();
    let DesiredGrants::Present { inbounds } = &first.targets[0].desired.grants else {
        panic!("期望 Present");
    };
    assert!(
        inbounds[0].clients.iter().any(|c| c.flow.is_some()),
        "fixture 要带 flow，否则这条测试测不到东西"
    );

    // The shape an agent reports, written by hand rather than copied from desired.
    //
    // The probe credential is in this list too: it ships down the same channel as a real user
    // (`physical/node.rs`), so "added correctly on the machine" must include it — without it a
    // credential really is missing and it should judge that a sync is needed. The UUID is computed
    // explicitly through `probe_uuid`, pinning down that it is genuinely derived.
    let observed = AppliedGrantsState::Present {
        inbounds: vec![ObservedInbound {
            tag: "in:direct/i-hk".to_owned(),
            clients: vec![
                ObservedClient {
                    email: "alice@platform.acme#i-hk".to_owned(),
                    uuid: "uuid-alice".to_owned(),
                    flow: Some("xtls-rprx-vision".to_owned()),
                },
                ObservedClient {
                    email: brocade_core::model::probe_label("i-hk"),
                    uuid: brocade_core::model::probe_uuid("priv-i-hk", "i-hk"),
                    flow: Some("xtls-rprx-vision".to_owned()),
                },
            ],
        }],
    };
    let applied = vec![NodeAppliedState {
        hy2_port_hop: AppliedArtifactState::Unmanaged,
        running_xray: None,
        phantun: AppliedArtifactState::Unmanaged,
        node_id: "hk".to_owned(),
        wireguard: applied_artifact(&first.targets[0].desired.wireguard),
        xray: applied_artifact(&first.targets[0].desired.xray),
        grants: observed,
    }];

    let plan = plan_deployment(&snapshot, &applied).unwrap();
    assert_eq!(plan.targets[0].actions, Vec::new());
    assert_eq!(plan.targets[0].status, PlannedTargetStatus::Skipped);
}

/// A changed uuid with an unchanged email must still call for action — the test is not email
/// alone.
#[test]
fn grants_need_action_when_uuid_changed_under_the_same_email() {
    let snapshot = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app("direct", "hk", "i-hk", 8443, true, any_egress())],
    );
    let first = plan_deployment(&snapshot, &[]).unwrap();
    let applied = vec![NodeAppliedState {
        hy2_port_hop: AppliedArtifactState::Unmanaged,
        running_xray: None,
        phantun: AppliedArtifactState::Unmanaged,
        node_id: "hk".to_owned(),
        wireguard: applied_artifact(&first.targets[0].desired.wireguard),
        xray: applied_artifact(&first.targets[0].desired.xray),
        grants: AppliedGrantsState::Present {
            inbounds: vec![ObservedInbound {
                tag: "in:direct/i-hk".to_owned(),
                clients: vec![ObservedClient {
                    email: "alice@platform.acme#i-hk".to_owned(),
                    uuid: "uuid-alice-OLD".to_owned(),
                    flow: Some("xtls-rprx-vision".to_owned()),
                }],
            }],
        },
    }];

    let plan = plan_deployment(&snapshot, &applied).unwrap();
    assert_eq!(plan.targets[0].actions, vec![PlannedAction::SyncGrants]);
}

/// A changed flow with unchanged email and uuid must reload the user too. VLESS flow is per-client
/// configuration and xray's `inbounduser` reads it back.
#[test]
fn grants_need_action_when_flow_changed_under_the_same_email_and_uuid() {
    let snapshot = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app("direct", "hk", "i-hk", 8443, true, any_egress())],
    );
    let first = plan_deployment(&snapshot, &[]).unwrap();
    let applied = vec![NodeAppliedState {
        hy2_port_hop: AppliedArtifactState::Unmanaged,
        running_xray: None,
        phantun: AppliedArtifactState::Unmanaged,
        node_id: "hk".to_owned(),
        wireguard: applied_artifact(&first.targets[0].desired.wireguard),
        xray: applied_artifact(&first.targets[0].desired.xray),
        grants: AppliedGrantsState::Present {
            inbounds: vec![ObservedInbound {
                tag: "in:direct/i-hk".to_owned(),
                clients: vec![ObservedClient {
                    email: "alice@platform.acme#i-hk".to_owned(),
                    uuid: "uuid-alice".to_owned(),
                    flow: None,
                }],
            }],
        },
    }];

    let plan = plan_deployment(&snapshot, &applied).unwrap();
    assert_eq!(plan.targets[0].actions, vec![PlannedAction::SyncGrants]);
}

/// With an empty or `Unknown` baseline, `Disabled` still calls for action. Otherwise it deadlocks:
/// nothing is sent so nothing is observed, nothing is observed so nothing is ever sent, and the
/// machine's old config and old private keys remain.
#[test]
fn disabled_artifacts_act_against_an_unknown_baseline() {
    let snapshot = snapshot(vec![edge_node("edge", [10, 66, 0, 9])], Vec::new());

    for applied in [Vec::new(), vec![NodeAppliedState::unknown("edge")]] {
        let plan = plan_deployment(&snapshot, &applied).unwrap();
        let target = &plan.targets[0];
        assert!(
            matches!(target.desired.xray, DesiredArtifact::Disabled { .. })
                && matches!(target.desired.wireguard, DesiredArtifact::Disabled { .. }),
            "这台机器既不进 overlay 也不在任何应用视图里"
        );
        assert_eq!(
            target.actions,
            vec![
                PlannedAction::DisablePhantun,
                PlannedAction::DisableHy2PortHop,
                PlannedAction::DisableWireGuard,
                PlannedAction::DisableXray,
            ]
        );
        assert_eq!(target.status, PlannedTargetStatus::Pending);
    }
}

/// Once `Disabled` is confirmed the action is not repeated.
#[test]
fn disabled_artifacts_settle_once_observed_disabled() {
    let snapshot = snapshot(vec![edge_node("edge", [10, 66, 0, 9])], Vec::new());
    let applied = vec![NodeAppliedState {
        hy2_port_hop: AppliedArtifactState::Unmanaged,
        running_xray: None,
        phantun: AppliedArtifactState::Unmanaged,
        node_id: "edge".to_owned(),
        wireguard: AppliedArtifactState::Disabled,
        xray: AppliedArtifactState::Disabled,
        grants: AppliedGrantsState::Disabled,
    }];

    let plan = plan_deployment(&snapshot, &applied).unwrap();
    assert_eq!(plan.targets[0].actions, Vec::new());
    assert_eq!(plan.targets[0].status, PlannedTargetStatus::Skipped);
}

/// Narrowing must keep the artifact the plan says it is moving.
///
/// Dropping one is silent in every direction: the agent leaves the machine alone on `Unmanaged`
/// and reports `Unmanaged` back, the control plane keeps the previous applied state because this
/// deployment did not manage that artifact, and the next plan asks for the same action. The
/// operator releases, the release succeeds, the machine is still pending — forever. Port hopping
/// shipped narrowed under phantun's actions and produced exactly that on a machine whose only
/// change was a hop range.
#[test]
fn a_hop_only_release_still_carries_the_hop_artifact() {
    let snapshot = snapshot(
        vec![tls_node("hk", "hk.example.net", [10, 66, 0, 1])],
        vec![hopping_app("direct", "hk", "i-direct", 8443, any_egress())],
    );
    // Everything on this machine already matches, except the hop rules, whose baseline is the
    // `Unknown` every machine's column starts in.
    let applied = vec![NodeAppliedState {
        hy2_port_hop: AppliedArtifactState::Unknown,
        ..applied_from_plan(&plan_deployment(&snapshot, &[]).unwrap()).remove(0)
    }];

    let plan = plan_deployment(&snapshot, &applied).unwrap();
    assert_eq!(
        plan.targets[0].actions,
        vec![PlannedAction::ApplyHy2PortHop]
    );

    let narrowed = narrow_to_kind(plan, DeploymentKind::Config);

    assert!(
        matches!(
            narrowed.targets[0].desired.hy2_port_hop,
            DesiredArtifact::Present { .. }
        ),
        "这一单唯一要做的就是装跳转规则，收窄后却不下发：{:?}",
        narrowed.targets[0].desired.hy2_port_hop
    );
}

/// That observation payload must be accepted verbatim. The observation side used to reuse the
/// desired side's type and errored outright on a missing `level`, and `load_applied_states` collects
/// in one pass, so one machine's observation failing to parse left the whole fleet without a plan.
/// An older observation without flow reads as None.
#[test]
fn observation_payload_from_the_spec_deserializes() {
    let payload = r#"[{
        "tag": "in:app/i-hk443",
        "clients": [{ "email": "alice@platform.acme#i-hk443", "uuid": "2d2304da-f114-4574-8d44-625afdb1db5c" }]
    }]"#;

    let inbounds: Vec<ObservedInbound> = serde_json::from_str(payload).unwrap();
    assert_eq!(inbounds[0].tag, "in:app/i-hk443");
    assert_eq!(inbounds[0].clients[0].email, "alice@platform.acme#i-hk443");
    assert_eq!(inbounds[0].clients[0].flow, None);
}

#[test]
fn a_retired_node_remains_a_teardown_target_until_all_artifacts_are_disabled() {
    let before = snapshot(
        vec![
            node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System),
            node("sg", "sg.example.net", [10, 66, 0, 2], Dns::System),
        ],
        vec![direct_app(
            "direct",
            "hk",
            "i-direct",
            8443,
            true,
            any_egress(),
        )],
    );
    let applied = applied_from_plan(&plan_deployment(&before, &[]).unwrap());

    let mut after = before.clone();
    after
        .nodes
        .iter_mut()
        .find(|n| n.id == "sg")
        .unwrap()
        .retired = true;
    let plan = plan_deployment(&after, &applied).unwrap();

    let sg = plan
        .targets
        .iter()
        .find(|target| target.node_id == "sg")
        .expect("未确认清理的退役机器必须保留为 teardown target");
    assert_eq!(sg.status, PlannedTargetStatus::Pending);
    assert!(matches!(
        &sg.desired.phantun,
        DesiredArtifact::Disabled { .. }
    ));
    assert!(matches!(
        &sg.desired.hy2_port_hop,
        DesiredArtifact::Disabled { .. }
    ));
    assert!(matches!(
        &sg.desired.wireguard,
        DesiredArtifact::Disabled { .. }
    ));
    assert!(matches!(&sg.desired.xray, DesiredArtifact::Disabled { .. }));

    let hk = plan.targets.iter().find(|t| t.node_id == "hk").unwrap();
    assert_eq!(hk.status, PlannedTargetStatus::Pending);
    if let DesiredArtifact::Present { content, .. } = &hk.desired.wireguard {
        assert!(
            !content.contains("sg"),
            "退役的机器还留在别人的对端表里：{content}"
        );
    } else {
        panic!("hk 该有 wg 配置");
    }

    let converged = applied
        .into_iter()
        .map(|state| {
            if state.node_id != "sg" {
                return state;
            }
            NodeAppliedState {
                phantun: AppliedArtifactState::Disabled,
                hy2_port_hop: AppliedArtifactState::Disabled,
                wireguard: AppliedArtifactState::Disabled,
                xray: AppliedArtifactState::Disabled,
                grants: AppliedGrantsState::Disabled,
                ..state
            }
        })
        .collect::<Vec<_>>();
    let settled = plan_deployment(&after, &converged).unwrap();
    assert!(
        settled.targets.iter().all(|target| target.node_id != "sg"),
        "全组件已确认停用后，后续发布才应排除退役机器"
    );
}

fn applied_from_plan(plan: &DeploymentPlan) -> Vec<NodeAppliedState> {
    plan.targets
        .iter()
        .map(|target| NodeAppliedState {
            hy2_port_hop: AppliedArtifactState::Unmanaged,
            running_xray: None,
            node_id: target.node_id.clone(),
            phantun: applied_artifact(&target.desired.phantun),
            wireguard: applied_artifact(&target.desired.wireguard),
            xray: applied_artifact(&target.desired.xray),
            grants: applied_grants(&target.desired.grants),
        })
        .collect()
}

fn applied_artifact(desired: &DesiredArtifact) -> AppliedArtifactState {
    match desired {
        DesiredArtifact::Present { sha256, .. } => AppliedArtifactState::Present {
            sha256: sha256.clone(),
        },
        DesiredArtifact::Disabled { .. } => AppliedArtifactState::Disabled,
        DesiredArtifact::Unmanaged { .. } => AppliedArtifactState::Unmanaged,
    }
}

/// Simulates the observation an agent reports after converging. It must be lossy: `inbounduser`
/// yields `(email, uuid, flow)` and not level. This helper used to copy the desired state wholesale,
/// so observed and desired were forever byte-for-byte equal and a mistaken comparison went
/// untested.
fn applied_grants(desired: &DesiredGrants) -> AppliedGrantsState {
    match desired {
        DesiredGrants::Present { inbounds } => AppliedGrantsState::Present {
            inbounds: observed_from_desired(inbounds),
        },
        DesiredGrants::Disabled { .. } => AppliedGrantsState::Disabled,
        DesiredGrants::Unmanaged { .. } => AppliedGrantsState::Unmanaged,
    }
}

fn observed_from_desired(inbounds: &[GrantInbound]) -> Vec<ObservedInbound> {
    inbounds
        .iter()
        .map(|inbound| ObservedInbound {
            tag: inbound.tag.clone(),
            clients: inbound
                .clients
                .iter()
                .map(|client| ObservedClient {
                    email: client.email.clone(),
                    uuid: client.uuid.clone(),
                    flow: client.flow.clone(),
                })
                .collect(),
        })
        .collect()
}

fn target<'a>(plan: &'a DeploymentPlan, node_id: &str) -> &'a PlannedTarget {
    plan.targets
        .iter()
        .find(|target| target.node_id == node_id)
        .unwrap()
}

fn snapshot(nodes: Vec<Node>, apps: Vec<AppView>) -> ModelSnapshot {
    ModelSnapshot {
        revision: 91,
        overlay_cidr: Ipv4Net::new(Ipv4Addr::new(10, 66, 0, 0), 16).unwrap(),
        settings: Default::default(),
        nodes,
        node_egress_dns: Vec::new(),
        users: vec![User {
            tenant: "platform.acme".to_owned(),
            id: "alice".to_owned(),
            uuid: "uuid-alice".to_owned(),
        }],
        external_outbounds: Vec::new(),
        apps,
    }
}

fn node(id: &str, public_ipv4: &str, overlay: [u8; 4], dns: Dns) -> Node {
    Node {
        mtu: None,
        connection: Default::default(),
        id: id.to_owned(),
        tenant: "platform.acme".to_owned(),
        name: id.to_owned(),
        public_ipv4: Some(public_ipv4.to_owned()),
        public_ipv6: None,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        overlay_addr: Ipv4Addr::from(overlay),
        certificate_name: None,
        wireguard: WireGuardKeys {
            private_key: format!("priv-{id}"),
            public_key: format!("pub-{id}"),
            listen_port: 51820,
            transport: Default::default(),
        },
        api_port: Some(10085),
        overlay: true,
        egress_allowed: true,
        dns,
        domain_strategy: DomainStrategy::default(),
        retired: false,
    }
}

/// A machine holding a certificate, which every shape presenting one of its own requires —
/// Hysteria 2 among them.
fn tls_node(id: &str, public_ipv4: &str, overlay: [u8; 4]) -> Node {
    Node {
        certificate_name: Some(format!("{id}.example.net")),
        ..node(id, public_ipv4, overlay, Dns::System)
    }
}

/// A machine off the backbone. Combined with being in no application view, both artifacts come out
/// `Disabled`.
fn edge_node(id: &str, overlay: [u8; 4]) -> Node {
    Node {
        overlay: false,
        ..node(id, &format!("{id}.example.net"), overlay, Dns::System)
    }
}

fn direct_app(
    id: &str,
    node_id: &str,
    ingress_id: &str,
    port: u16,
    with_grant: bool,
    default_rule: Rule,
) -> AppView {
    let chain_id = format!("c-{id}");
    AppView {
        id: id.to_owned(),
        label: id.to_owned(),
        chains: vec![Chain {
            id: chain_id.clone(),
            tenant: "platform.acme".to_owned(),
            name: chain_id.clone(),
            subscription_country: None,
        }],
        ingresses: vec![Ingress {
            id: ingress_id.to_owned(),
            chain: chain_id.clone(),
            node: node_id.to_owned(),
            bind: IpAddr::from(Ipv4Addr::UNSPECIFIED),
            port,
            front: None,
            projection: Default::default(),
            guard: brocade_core::model::IngressGuard::OPEN,
            identity: brocade_core::model::IngressIdentity {
                private_key: format!("priv-{ingress_id}"),
                public_key: format!("pub-{ingress_id}"),
                short_ids: vec!["0123abcd".to_owned()],
            },
            wires: IngressWires::Vless(Transport::VlessReality(
                brocade_core::model::RealitySettings {
                    dest: "www.example.com:443".to_owned(),
                    server_names: vec!["www.example.com".to_owned()],
                    fingerprint: "chrome".to_owned(),
                    // Ingresses in production all carry flow (see pg_integration's fixture) and the
                    // observation side must carry it back. Left None, the "unchanged should skip"
                    // assertions below all spin for nothing.
                    flow: Some("xtls-rprx-vision".to_owned()),
                    fallback_mode: Default::default(),
                    fallback_guard: true,
                    fallback_limits: Default::default(),
                },
            )),
        }],
        fronts: Vec::new(),
        steps: vec![Step {
            chain: chain_id,
            node: node_id.to_owned(),
            accept: None,
            hop_in: None,
            rules: vec![default_rule],
        }],
        grants: if with_grant {
            vec![Grant {
                tenant: "platform.acme".to_owned(),
                user: "alice".to_owned(),
                ingress: ingress_id.to_owned(),
            }]
        } else {
            Vec::new()
        },
    }
}

/// The same ingress with a Hysteria 2 half beside its VLESS one, hopping over a range. The range
/// is what produces a port-hop artifact for the machine.
fn hopping_app(
    id: &str,
    node_id: &str,
    ingress_id: &str,
    port: u16,
    default_rule: Rule,
) -> AppView {
    let mut app = direct_app(id, node_id, ingress_id, port, true, default_rule);
    let ingress = &mut app.ingresses[0];
    let vless = ingress
        .wires
        .vless()
        .expect("direct_app 建的是 VLESS")
        .clone();
    ingress.wires = IngressWires::Both {
        vless,
        hysteria2: Hysteria2 {
            port: 50_001,
            hop: Some(HysteriaPortHop {
                start: 50_001,
                end: 50_010,
            }),
            ..Hysteria2::default()
        },
    };
    app
}

fn any_egress() -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Egress { send_through: None },
    }
}

/// A rule-table edit is installable into a running xray, so the release that carries it drops
/// nothing — and a plan that called it destructive would wave it and stop for a confirmation
/// nobody owes.
///
/// The control plane may only say so when it can prove what the machine is running, which is
/// what `running_xray` carries. The proof itself lives in the store (the text is recompiled
/// from the baseline revision and accepted only if it hashes to what the machine reported);
/// here it is handed in directly, so that what is under test is the judgement rather than the
/// plumbing.
#[test]
fn a_change_the_running_xray_can_absorb_is_not_disruptive() {
    let before = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app("direct", "hk", "i-hk", 8443, true, any_egress())],
    );
    // Same machine, same ingress, same port — only where the traffic is sent.
    let after = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app(
            "direct",
            "hk",
            "i-hk",
            8443,
            true,
            Rule {
                dest_match: DestMatch::DomainSuffix(vec!["example.com".to_owned()]),
                action: Action::Egress { send_through: None },
            },
        )],
    );

    let first = plan_deployment(&before, &[]).unwrap();
    let DesiredArtifact::Present { content, .. } = &first.targets[0].desired.xray else {
        panic!("期望 hk 有 xray 制品");
    };
    let running = content.clone();

    let applied = vec![NodeAppliedState {
        hy2_port_hop: AppliedArtifactState::Unmanaged,
        running_xray: Some(running.clone()),
        node_id: "hk".to_owned(),
        phantun: AppliedArtifactState::Unmanaged,
        wireguard: applied_artifact(&first.targets[0].desired.wireguard),
        xray: applied_artifact(&first.targets[0].desired.xray),
        grants: AppliedGrantsState::Unmanaged,
    }];

    let plan = plan_deployment(&after, &applied).unwrap();
    let hk = &plan.targets[0];
    assert!(
        hk.actions.contains(&PlannedAction::ApplyXray),
        "制品确实变了，动作还是 ApplyXray：{:?}",
        hk.actions
    );
    assert!(
        !hk.disruptive,
        "只动了路由表，agent 会热切进去，不该标成破坏性"
    );

    // The same release, judged for a machine whose running config the control plane cannot
    // vouch for. Nothing is known, so nothing is promised.
    let unproven = vec![NodeAppliedState {
        hy2_port_hop: AppliedArtifactState::Unmanaged,
        running_xray: None,
        ..applied[0].clone()
    }];
    assert!(
        plan_deployment(&after, &unproven).unwrap().targets[0].disruptive,
        "不知道机器在跑什么就必须按最坏算"
    );

    // And for one whose reported digest does not match the text — drifted, or never received
    // the release the baseline says it did.
    let stale = vec![NodeAppliedState {
        hy2_port_hop: AppliedArtifactState::Unmanaged,
        running_xray: Some(running),
        xray: AppliedArtifactState::Present {
            sha256: "0".repeat(64),
        },
        ..applied[0].clone()
    }];
    assert!(
        plan_deployment(&after, &stale).unwrap().targets[0].disruptive,
        "摘要对不上就等于不知道"
    );
}

#[test]
fn adding_a_managed_warp_outbound_requires_an_xray_restart() {
    let mut before = snapshot(
        vec![node("hk", "hk.example.net", [10, 66, 0, 1], Dns::System)],
        vec![direct_app("direct", "hk", "i-hk", 8443, true, any_egress())],
    );
    before.external_outbounds = vec![ExternalOutbound {
        id: "warp".to_owned(),
        tenant: "platform.acme".to_owned(),
        name: "WARP".to_owned(),
        address: "engage.example".to_owned(),
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
            node: "hk".to_owned(),
            device_id: "device-hk".to_owned(),
            account_id: "account-hk".to_owned(),
            registered_at: "2026-08-30T12:00:00Z".to_owned(),
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
            local_addresses: vec![
                "172.16.0.2/32".to_owned(),
                "2606:4700:110:8::2/128".to_owned(),
            ],
            reserved: vec![1, 2, 3],
        }],
    }];

    let baseline = plan_deployment(&before, &[]).unwrap();
    let DesiredArtifact::Present { content, .. } = &baseline.targets[0].desired.xray else {
        panic!("基线应生成 xray 配置");
    };
    let mut applied = applied_from_plan(&baseline);
    applied[0].running_xray = Some(content.clone());

    let mut after = before.clone();
    after.apps[0].steps[0].rules = vec![Rule {
        dest_match: DestMatch::Any,
        action: Action::Proxy {
            outbound: "warp".to_owned(),
        },
    }];

    let plan = plan_deployment(&after, &applied).unwrap();
    let hk = target(&plan, "hk");
    assert_eq!(hk.actions, vec![PlannedAction::ApplyXray]);
    assert!(
        hk.disruptive,
        "新增 WARP/WireGuard 出站不能通过 HandlerService 热切"
    );
}
