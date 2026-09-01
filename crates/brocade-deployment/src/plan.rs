use std::collections::{BTreeMap, BTreeSet};

use brocade_core::{
    artifacts::{
        grants, hy2_port_hop, hy2_port_hop::Hy2PortHopArtifact, phantun, phantun::PhantunArtifact,
        wireguard, wireguard::WireGuardArtifact, xray, xray::XrayArtifact,
    },
    compile::{self, PublishBlocked},
    diagnostic::Level,
    format::{ini, json},
    hash::sha256_hex,
    model::ModelSnapshot,
};
use serde::{Deserialize, Serialize};

use crate::hotswap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentPlan {
    pub revision: u64,
    pub targets: Vec<PlannedTarget>,
    pub summary: PlanSummary,
    pub warnings: Vec<PlanDiagnostic>,
    // The preview page needs the baseline for what this deployment turns the artifacts into: the
    // same meaning as the one fixed at creation — the revision this kind last shipped
    // successfully. Purely computational paths (quota evaluation, the agent's desired
    // computation) query no database and leave this None, filled in by the store layer.
    #[serde(default)]
    pub base_revision_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSummary {
    pub total_targets: usize,
    pub changed_targets: usize,
    pub skipped_targets: usize,
    pub disruptive_targets: usize,
    pub max_wave: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDiagnostic {
    pub code: String,
    pub location: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTarget {
    pub node_id: String,
    pub status: PlannedTargetStatus,
    pub wave: u32,
    pub disruptive: bool,
    pub actions: Vec<PlannedAction>,
    pub desired: NodeDesiredState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlannedTargetStatus {
    Pending,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlannedAction {
    ApplyPhantun,
    ApplyHy2PortHop,
    ApplyWireGuard,
    ApplyXray,
    SyncGrants,
    DisablePhantun,
    DisableHy2PortHop,
    DisableWireGuard,
    DisableXray,
}

/// The four things a configuration deployment writes to a machine.
///
/// Every stage that has to walk them — narrowing, storing their contents, recording what was
/// asked for, judging what came back — walks this list rather than naming the four fields
/// again. Naming them again is how one gets left out, and each omission fails silently in its
/// own way: narrowing sends `Unmanaged` and the machine is never touched; storing skips the
/// blob and the machine's own poll errors; judging ignores it and a release that changed
/// nothing reports success. Port hopping shipped and hit all three at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigArtifact {
    Phantun,
    /// Hysteria 2's UDP hop range, as nft rules on the machine — not a chain's relay hop, which
    /// is a TCP inbound and is not an artifact of its own at all (it lives inside `xray`).
    Hy2PortHop,
    WireGuard,
    Xray,
}

impl ConfigArtifact {
    /// All four, for stages that hold only the kinds. `NodeDesiredState::artifacts` returns them
    /// in this order beside their values.
    pub const ALL: [ConfigArtifact; 4] = [
        ConfigArtifact::Phantun,
        ConfigArtifact::Hy2PortHop,
        ConfigArtifact::WireGuard,
        ConfigArtifact::Xray,
    ];

    /// Its name in `deployment_target_state.desired_structure`. The one place the wire name is
    /// written, so what is stored and what is read back cannot drift apart.
    pub fn field(self) -> &'static str {
        match self {
            ConfigArtifact::Phantun => "phantun",
            ConfigArtifact::Hy2PortHop => "hy2_port_hop",
            ConfigArtifact::WireGuard => "wireguard",
            ConfigArtifact::Xray => "xray",
        }
    }

    /// Whether a release record predating this artifact may leave it out.
    ///
    /// True for phantun and Hysteria 2's hop range, the two that arrived after release records
    /// were already being written. Rolling back to a record written before they existed
    /// must not suddenly touch them, so their absence reads as `Unmanaged` rather than as a
    /// corrupt record. The other two have been in every record ever written, and their absence
    /// is corruption — worth an error rather than a silent "not mine to manage".
    pub fn predates_records(self) -> bool {
        matches!(self, ConfigArtifact::Phantun | ConfigArtifact::Hy2PortHop)
    }
}

impl PlannedAction {
    /// Which artifact this action moves, or `None` for one that moves no artifact at all.
    ///
    /// `SyncGrants` is the `None`: the list goes into a running xray over its API, writing no
    /// file and restarting nothing, so there is no desired field for narrowing to keep on its
    /// account.
    fn artifact(self) -> Option<ConfigArtifact> {
        match self {
            PlannedAction::ApplyPhantun | PlannedAction::DisablePhantun => {
                Some(ConfigArtifact::Phantun)
            }
            PlannedAction::ApplyHy2PortHop | PlannedAction::DisableHy2PortHop => {
                Some(ConfigArtifact::Hy2PortHop)
            }
            PlannedAction::ApplyWireGuard | PlannedAction::DisableWireGuard => {
                Some(ConfigArtifact::WireGuard)
            }
            PlannedAction::ApplyXray | PlannedAction::DisableXray => Some(ConfigArtifact::Xray),
            PlannedAction::SyncGrants => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDesiredState {
    /// Coming before wireguard is not a matter of layout: `wg0.conf`'s `Endpoint` points at the
    /// phantun client's local port, and with phantun not up the handshake packets go to a port
    /// nobody listens on — the symptom being "everything configured correctly, handshake simply
    /// will not complete". This is the convergence order.
    #[serde(default = "unmanaged_artifact")]
    pub phantun: DesiredArtifact,
    pub wireguard: DesiredArtifact,
    pub xray: DesiredArtifact,
    #[serde(default = "unmanaged_artifact_hop")]
    pub hy2_port_hop: DesiredArtifact,
    pub grants: DesiredGrants,
}

impl NodeDesiredState {
    /// The four artifacts, each beside which one it is. Grants are not among them: they carry no
    /// file and no digest, and every stage that walks this list is about files.
    pub fn artifacts(&self) -> [(ConfigArtifact, &DesiredArtifact); 4] {
        [
            (ConfigArtifact::Phantun, &self.phantun),
            (ConfigArtifact::Hy2PortHop, &self.hy2_port_hop),
            (ConfigArtifact::WireGuard, &self.wireguard),
            (ConfigArtifact::Xray, &self.xray),
        ]
    }

    pub fn artifacts_mut(&mut self) -> [(ConfigArtifact, &mut DesiredArtifact); 4] {
        [
            (ConfigArtifact::Phantun, &mut self.phantun),
            (ConfigArtifact::Hy2PortHop, &mut self.hy2_port_hop),
            (ConfigArtifact::WireGuard, &mut self.wireguard),
            (ConfigArtifact::Xray, &mut self.xray),
        ]
    }
}

/// An older agent's report has no phantun field, defaulting to "not mine to manage" rather than
/// "absent" — the latter has the control plane judge it as drift and push an action every round
/// that the agent will never perform.
fn unmanaged_applied() -> AppliedArtifactState {
    AppliedArtifactState::Unmanaged
}

fn unmanaged_artifact_hop() -> DesiredArtifact {
    DesiredArtifact::Unmanaged {
        reason: "agent does not report port hop".to_owned(),
    }
}

fn unmanaged_artifact() -> DesiredArtifact {
    DesiredArtifact::Unmanaged {
        reason: "agent does not report phantun".to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum DesiredArtifact {
    Present { content: String, sha256: String },
    Disabled { reason: String },
    Unmanaged { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum DesiredGrants {
    Present { inbounds: Vec<GrantInbound> },
    Disabled { reason: String },
    Unmanaged { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GrantInbound {
    pub tag: String,
    pub clients: Vec<GrantClient>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GrantClient {
    pub email: String,
    pub uuid: String,
    pub flow: Option<String>,
    pub level: u8,
}

/// The grants read back from a machine. `uuid` is the generic credential slot: VLESS
/// `inbounduser` returns `account.id`, while Hysteria 2 returns `account.auth`. Neither protocol
/// reports `level`; the convergence test therefore excludes it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservedInbound {
    pub tag: String,
    pub clients: Vec<ObservedClient>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservedClient {
    pub email: String,
    pub uuid: String,
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAppliedState {
    pub node_id: String,
    /// An older agent does not report this field, defaulting to Unmanaged — taken as "not mine to
    /// manage" rather than "absent", since the latter has the control plane judge it as drift and
    /// push an action every round that the agent will never perform.
    #[serde(default = "unmanaged_applied")]
    pub phantun: AppliedArtifactState,
    #[serde(default = "unmanaged_applied")]
    pub hy2_port_hop: AppliedArtifactState,
    pub wireguard: AppliedArtifactState,
    pub xray: AppliedArtifactState,
    pub grants: AppliedGrantsState,
    /// The xray config this machine is actually running, where the control plane can prove
    /// which one that is.
    ///
    /// `xray` above records only a digest, which answers "has it changed" and nothing else.
    /// Whether a change can be installed into the running process depends on *what* changed,
    /// so the comparison needs the previous text — and getting it wrong in the permissive
    /// direction means promising an operator that a release drops nothing and then dropping
    /// every connection on the machine. Absent whenever that proof is missing, which is read
    /// as "assume the worst".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_xray: Option<String>,
}

impl NodeAppliedState {
    pub fn unknown(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            phantun: AppliedArtifactState::Unknown,
            hy2_port_hop: AppliedArtifactState::Unknown,
            wireguard: AppliedArtifactState::Unknown,
            xray: AppliedArtifactState::Unknown,
            grants: AppliedGrantsState::Unknown,
            running_xray: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum AppliedArtifactState {
    Present { sha256: String },
    Disabled,
    Unmanaged,
    Unknown,
    Dirty { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum AppliedGrantsState {
    Present { inbounds: Vec<ObservedInbound> },
    Disabled,
    Unmanaged,
    Unknown,
    Dirty { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    PublishBlocked(PublishBlocked),
}

// One deployment does one kind of thing.
// The two kinds differ in cost by an order of magnitude: configuration lands on disk and restarts
// a process or reconnects a tunnel; grants merely add to or remove from the list inside a running
// xray over an API, with the process untouched. Mixing them in one deployment costs more than
// tidiness — a grant change would restart xray along the way, because the agent converges on the
// desired state without looking at actions, and an xray that is Present in desired gets a pkill
// and a restart even where the deployment changes one person's grant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeploymentKind {
    /// A configuration deployment: artifacts land on disk and a process restarts or a tunnel
    /// reconnects. Where it restarts xray it aligns the grants along the way — a restart empties
    /// the runtime `clients` (the config file's are always empty, see artifacts/xray.rs), and
    /// without reloading them in the same deployment nobody on that machine can connect again.
    /// Configuration targets that leave xray running mark grants Unmanaged; permission-only work
    /// belongs to the automatic grants line.
    #[default]
    Config,
    /// A grants deployment: it moves only xray's runtime list, touching no config file and
    /// restarting no process.
    Grants,
}

impl DeploymentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeploymentKind::Config => "config",
            DeploymentKind::Grants => "grants",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "config" => Some(DeploymentKind::Config),
            "grants" => Some(DeploymentKind::Grants),
            _ => None,
        }
    }
}

/// Whether this machine can synchronize its list on its own right now.
///
/// The test is not "there is no work besides the list" but "xray owes nothing": the list goes into
/// an xray inbound and has nothing to do with wg's MTU or phantun's port. Where a machine owes both
/// an MTU change and a grant, there is no reason for the grant to wait with it — that is precisely
/// what was most painful while the deployment kinds were undivided.
///
/// An owed xray change does rule it out: the inbound set may not be built yet, and `sync_grants`
/// would error at `read_users` against a tag that does not exist. That is a physical constraint,
/// not caution.
pub fn can_sync_grants_now(target: &PlannedTarget) -> bool {
    target.status == PlannedTargetStatus::Pending
        && target.actions.contains(&PlannedAction::SyncGrants)
        && !target.actions.iter().any(|action| {
            matches!(
                action,
                PlannedAction::ApplyXray | PlannedAction::DisableXray
            )
        })
}

/// Narrow a full plan into a deployment of one kind.
///
/// Two things: pick the machines this deployment governs, and mark the artifacts it does not move
/// as `Unmanaged`. The latter is the crux — the agent's `converge_linux_*` returns on seeing
/// `Unmanaged` and applies unconditionally on seeing `Present`. Without narrowing, changing one
/// grant still restarts xray.
pub fn narrow_to_kind(mut plan: DeploymentPlan, kind: DeploymentKind) -> DeploymentPlan {
    match kind {
        DeploymentKind::Config => {
            // A configuration deployment still covers every machine, but each carries only the
            // artifacts it really moves. A machine that only changed wg should not get an xray
            // pkill thrown in.
            for target in &mut plan.targets {
                narrow_config_desired(target);
            }
        }
        DeploymentKind::Grants => {
            plan.targets.retain(can_sync_grants_now);
            for target in &mut plan.targets {
                target.desired.phantun = untouched("权限单不碰 phantun");
                target.desired.hy2_port_hop = untouched("权限单不碰端口跳转");
                target.desired.wireguard = untouched("权限单不碰 wireguard");
                // xray is marked Unmanaged too: the list goes into the running xray, and the
                // agent takes the inbounds and api port from the local `xray.json` without
                // rewriting the config or restarting.
                target.desired.xray = untouched("权限单不重启 xray");
            }
        }
    }
    assign_waves(&mut plan.targets);
    plan.summary = summarize_targets(&plan.targets);
    plan
}

/// Narrow one machine's desired state down to the artifacts this deployment really moves.
///
/// Each row pairs an artifact with its own field, because getting that pairing wrong does not
/// fail anywhere. A field wrongly marked `Unmanaged` goes out to the agent, which leaves the
/// machine alone and reports `Unmanaged` back, and the control plane keeps the old applied state
/// on the grounds that this deployment did not manage it (`upsert_node_applied_state`). The plan
/// then asks for the same action again. What the operator sees is a machine permanently pending
/// with every release succeeding and nothing on it ever changing — port hopping shipped gated on
/// phantun's actions and did exactly that.
fn narrow_config_desired(target: &mut PlannedTarget) {
    let manages_xray = target.actions.iter().any(|action| {
        matches!(
            action,
            PlannedAction::ApplyXray | PlannedAction::DisableXray
        )
    });
    let moved = target
        .actions
        .iter()
        .filter_map(|action| action.artifact())
        .collect::<Vec<_>>();
    for (artifact, desired) in target.desired.artifacts_mut() {
        if !moved.contains(&artifact) {
            *desired = untouched(&format!("这一单不动 {}", artifact.field()));
        }
    }
    if !manages_xray {
        // Runtime permissions belong to the automatic grants line.  A config release carries them
        // only when it restarts/replaces xray, because that operation empties the in-memory list
        // and must restore the release's own frozen snapshot before reporting success.
        target
            .actions
            .retain(|action| *action != PlannedAction::SyncGrants);
        target.desired.grants = DesiredGrants::Unmanaged {
            reason: "配置单未重启 xray，不改运行态权限".to_owned(),
        };
    }
    if target.actions.is_empty() {
        target.status = PlannedTargetStatus::Skipped;
        target.disruptive = false;
    }
}

fn untouched(reason: &str) -> DesiredArtifact {
    DesiredArtifact::Unmanaged {
        reason: reason.to_owned(),
    }
}

pub fn plan_deployment(
    snapshot: &ModelSnapshot,
    applied: &[NodeAppliedState],
) -> Result<DeploymentPlan, PlanError> {
    let output = compile::compile(snapshot);
    output
        .ensure_publishable()
        .map_err(PlanError::PublishBlocked)?;

    let warnings = output
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Warn)
        .map(|diagnostic| PlanDiagnostic {
            code: diagnostic.code.to_owned(),
            location: diagnostic.location.clone(),
            message: diagnostic.message.clone(),
        })
        .collect::<Vec<_>>();

    let retired = snapshot
        .nodes
        .iter()
        .filter(|node| node.retired)
        .map(|node| node.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut node_ids = snapshot
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    node_ids.sort_unstable();
    node_ids.dedup();

    let desired = node_ids
        .into_iter()
        .map(|node_id| {
            let node_plan = output
                .project_node(node_id)
                .map_err(PlanError::PublishBlocked)?;
            Ok((node_id.to_owned(), desired_state(&node_plan)))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut plan = plan_desired_deployment(snapshot.revision, desired, applied, warnings);
    let applied_by_node = applied
        .iter()
        .map(|state| (state.node_id.as_str(), state))
        .collect::<BTreeMap<_, _>>();
    for target in &mut plan.targets {
        if !retired.contains(target.node_id.as_str()) {
            continue;
        }
        // Outside retirement, Unmanaged explicitly means that Brocade must leave an artifact
        // alone. Retirement is the one stronger contract: absence of proof is not proof that a
        // listener, interface or rule is gone, so every non-Disabled observation gets one
        // idempotent disable and read-back.
        let applied = applied_by_node.get(target.node_id.as_str()).copied();
        target.actions = retirement_actions(&target.desired, applied);
        target.disruptive = target
            .actions
            .iter()
            .any(|action| is_disruptive(action, applied, &target.desired));
        target.status = if target.actions.is_empty() {
            PlannedTargetStatus::Skipped
        } else {
            PlannedTargetStatus::Pending
        };
    }
    // A retired machine remains a target only while teardown is owed. Once all managed artifacts
    // have been observed Disabled, carrying it forever in every future release adds noise and
    // falsely suggests that the machine still participates in the fleet.
    plan.targets.retain(|target| {
        !retired.contains(target.node_id.as_str()) || target.status == PlannedTargetStatus::Pending
    });
    assign_waves(&mut plan.targets);
    plan.summary = summarize_targets(&plan.targets);
    Ok(plan)
}

pub fn plan_forced_deployment(
    snapshot: &ModelSnapshot,
    applied: &[NodeAppliedState],
) -> Result<DeploymentPlan, PlanError> {
    let output = compile::compile(snapshot);
    output
        .ensure_publishable()
        .map_err(PlanError::PublishBlocked)?;

    let warnings = output
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Warn)
        .map(|diagnostic| PlanDiagnostic {
            code: diagnostic.code.to_owned(),
            location: diagnostic.location.clone(),
            message: diagnostic.message.clone(),
        })
        .collect::<Vec<_>>();
    let applied_by_node = applied
        .iter()
        .map(|state| (state.node_id.as_str(), state))
        .collect::<BTreeMap<_, _>>();

    let mut node_ids = snapshot
        .nodes
        .iter()
        .filter(|node| {
            !node.retired
                || !applied_by_node
                    .get(node.id.as_str())
                    .is_some_and(|state| retirement_artifacts_disabled(state))
        })
        .map(|node| node.id.as_str())
        .collect::<Vec<_>>();
    node_ids.sort_unstable();
    node_ids.dedup();

    let mut targets = node_ids
        .into_iter()
        .map(|node_id| {
            let node_plan = output
                .project_node(node_id)
                .map_err(PlanError::PublishBlocked)?;
            let desired = desired_state(&node_plan);
            let applied = applied_by_node.get(node_id).copied();
            let actions = forced_actions(&desired);
            let disruptive = actions
                .iter()
                .any(|action| is_disruptive(action, applied, &desired));
            let status = if actions.is_empty() {
                PlannedTargetStatus::Skipped
            } else {
                PlannedTargetStatus::Pending
            };
            Ok(PlannedTarget {
                node_id: node_id.to_owned(),
                status,
                wave: 0,
                disruptive,
                actions,
                desired,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    targets.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    assign_waves(&mut targets);
    let summary = summarize_targets(&targets);

    Ok(DeploymentPlan {
        revision: snapshot.revision,
        targets,
        summary,
        warnings,
        base_revision_id: None,
    })
}

pub fn plan_desired_deployment(
    revision: u64,
    desired: Vec<(String, NodeDesiredState)>,
    applied: &[NodeAppliedState],
    warnings: Vec<PlanDiagnostic>,
) -> DeploymentPlan {
    let applied_by_node = applied
        .iter()
        .map(|state| (state.node_id.as_str(), state))
        .collect::<BTreeMap<_, _>>();
    let mut targets = desired
        .into_iter()
        .map(|(node_id, desired)| {
            let applied = applied_by_node.get(node_id.as_str()).copied();
            let actions = planned_actions(&desired, applied);
            let disruptive = actions
                .iter()
                .any(|action| is_disruptive(action, applied, &desired));
            let status = if actions.is_empty() {
                PlannedTargetStatus::Skipped
            } else {
                PlannedTargetStatus::Pending
            };
            PlannedTarget {
                node_id,
                status,
                wave: 0,
                disruptive,
                actions,
                desired,
            }
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    assign_waves(&mut targets);
    let summary = summarize_targets(&targets);

    DeploymentPlan {
        revision,
        targets,
        summary,
        warnings,
        base_revision_id: None,
    }
}

fn desired_state(plan: &brocade_core::physical::node::NodePlan) -> NodeDesiredState {
    let phantun = desired_phantun(plan);
    let wireguard = desired_wireguard(plan);
    let xray = desired_xray(plan);
    let grants = desired_grants(plan, &xray);

    NodeDesiredState {
        phantun,
        wireguard,
        xray,
        hy2_port_hop: desired_hy2_port_hop(plan),
        grants,
    }
}

fn desired_hy2_port_hop(plan: &brocade_core::physical::node::NodePlan) -> DesiredArtifact {
    let artifact = hy2_port_hop::build(plan);
    match &artifact {
        Hy2PortHopArtifact::Config(_) => desired_present(json::hy2_port_hop(&artifact)),
        Hy2PortHopArtifact::Disabled { node_id } => DesiredArtifact::Disabled {
            reason: format!("{node_id} 这一版没有端口跳转"),
        },
    }
}

fn desired_phantun(plan: &brocade_core::physical::node::NodePlan) -> DesiredArtifact {
    let artifact = phantun::build(plan);
    match &artifact {
        PhantunArtifact::Config(_) => desired_present(json::phantun(&artifact)),
        PhantunArtifact::Disabled { node_id } => DesiredArtifact::Disabled {
            reason: format!("{node_id} 这一版不需要 phantun"),
        },
    }
}

fn desired_wireguard(plan: &brocade_core::physical::node::NodePlan) -> DesiredArtifact {
    let artifact = wireguard::build(plan);
    match &artifact {
        WireGuardArtifact::Config(_) => desired_present(ini::wireguard(&artifact)),
        WireGuardArtifact::Disabled { node_id } => DesiredArtifact::Disabled {
            reason: format!("{node_id} is not in the WireGuard overlay"),
        },
    }
}

fn desired_xray(plan: &brocade_core::physical::node::NodePlan) -> DesiredArtifact {
    let artifact = xray::build(plan);
    match &artifact {
        XrayArtifact::Config(_) => desired_present(json::xray(&artifact)),
        XrayArtifact::Disabled { node_id } => DesiredArtifact::Disabled {
            reason: format!("{node_id} has no xray workload in this revision"),
        },
    }
}

fn desired_grants(
    plan: &brocade_core::physical::node::NodePlan,
    desired_xray: &DesiredArtifact,
) -> DesiredGrants {
    if !matches!(desired_xray, DesiredArtifact::Present { .. }) {
        return DesiredGrants::Disabled {
            reason: "xray is disabled on this node".to_owned(),
        };
    }

    let batch = grants::build(plan);
    DesiredGrants::Present {
        inbounds: grant_inbounds(batch.inbounds),
    }
}

fn desired_present(content: String) -> DesiredArtifact {
    let sha256 = sha256_hex(content.as_bytes());
    DesiredArtifact::Present { content, sha256 }
}

fn grant_inbounds(inbounds: Vec<grants::GrantInboundUpdate>) -> Vec<GrantInbound> {
    let mut inbounds = inbounds
        .into_iter()
        .map(|inbound| {
            let mut clients = inbound
                .clients
                .into_iter()
                .map(|client| GrantClient {
                    email: client.label,
                    uuid: client.uuid,
                    flow: client.flow,
                    level: client.level,
                })
                .collect::<Vec<_>>();
            clients.sort();

            GrantInbound {
                tag: inbound.inbound_tag,
                clients,
            }
        })
        .collect::<Vec<_>>();
    inbounds.sort();
    inbounds
}

fn planned_actions(
    desired: &NodeDesiredState,
    applied: Option<&NodeAppliedState>,
) -> Vec<PlannedAction> {
    let mut actions = Vec::new();
    if artifact_needs_action(&desired.phantun, applied.map(|state| &state.phantun)) {
        actions.push(match desired.phantun {
            DesiredArtifact::Present { .. } => PlannedAction::ApplyPhantun,
            DesiredArtifact::Disabled { .. } => PlannedAction::DisablePhantun,
            DesiredArtifact::Unmanaged { .. } => unreachable!("unmanaged artifacts do not act"),
        });
    }
    if artifact_needs_action(&desired.wireguard, applied.map(|state| &state.wireguard)) {
        actions.push(match desired.wireguard {
            DesiredArtifact::Present { .. } => PlannedAction::ApplyWireGuard,
            DesiredArtifact::Disabled { .. } => PlannedAction::DisableWireGuard,
            DesiredArtifact::Unmanaged { .. } => unreachable!("unmanaged artifacts do not act"),
        });
    }
    if artifact_needs_action(&desired.xray, applied.map(|state| &state.xray)) {
        actions.push(match desired.xray {
            DesiredArtifact::Present { .. } => PlannedAction::ApplyXray,
            DesiredArtifact::Disabled { .. } => PlannedAction::DisableXray,
            DesiredArtifact::Unmanaged { .. } => unreachable!("unmanaged artifacts do not act"),
        });
    }
    if artifact_needs_action(
        &desired.hy2_port_hop,
        applied.map(|state| &state.hy2_port_hop),
    ) {
        actions.push(match desired.hy2_port_hop {
            DesiredArtifact::Present { .. } => PlannedAction::ApplyHy2PortHop,
            DesiredArtifact::Disabled { .. } => PlannedAction::DisableHy2PortHop,
            DesiredArtifact::Unmanaged { .. } => unreachable!("unmanaged artifacts do not act"),
        });
    }
    if grants_need_action(&desired.grants, applied.map(|state| &state.grants)) {
        actions.push(PlannedAction::SyncGrants);
    }
    actions.sort();
    actions
}

fn forced_actions(desired: &NodeDesiredState) -> Vec<PlannedAction> {
    let mut actions = Vec::new();
    match desired.phantun {
        DesiredArtifact::Present { .. } => actions.push(PlannedAction::ApplyPhantun),
        DesiredArtifact::Disabled { .. } => actions.push(PlannedAction::DisablePhantun),
        DesiredArtifact::Unmanaged { .. } => {}
    }
    match desired.wireguard {
        DesiredArtifact::Present { .. } => actions.push(PlannedAction::ApplyWireGuard),
        DesiredArtifact::Disabled { .. } => actions.push(PlannedAction::DisableWireGuard),
        DesiredArtifact::Unmanaged { .. } => {}
    }
    match desired.xray {
        DesiredArtifact::Present { .. } => actions.push(PlannedAction::ApplyXray),
        DesiredArtifact::Disabled { .. } => actions.push(PlannedAction::DisableXray),
        DesiredArtifact::Unmanaged { .. } => {}
    }
    match desired.hy2_port_hop {
        DesiredArtifact::Present { .. } => actions.push(PlannedAction::ApplyHy2PortHop),
        DesiredArtifact::Disabled { .. } => actions.push(PlannedAction::DisableHy2PortHop),
        DesiredArtifact::Unmanaged { .. } => {}
    }
    if matches!(desired.grants, DesiredGrants::Present { .. }) {
        actions.push(PlannedAction::SyncGrants);
    }
    actions.sort();
    actions
}

fn artifact_needs_action(
    desired: &DesiredArtifact,
    applied: Option<&AppliedArtifactState>,
) -> bool {
    match desired {
        DesiredArtifact::Unmanaged { .. } => false,
        DesiredArtifact::Present { sha256, .. } => match applied {
            Some(AppliedArtifactState::Present {
                sha256: applied_sha,
            }) => applied_sha != sha256,
            Some(AppliedArtifactState::Unmanaged) => false,
            Some(AppliedArtifactState::Disabled | AppliedArtifactState::Unknown) | None => true,
            Some(AppliedArtifactState::Dirty { .. }) => true,
        },
        // Both `Unknown` and "no record" call for action. An empty baseline is every machine's
        // normal state before its first release, and judging it as no action deadlocks: nothing
        // is sent so nothing is observed, nothing is observed so nothing is ever sent, and the
        // machine's old config, old private keys, and old listening ports remain — the very hole
        // the three states were split to close, and using `Unknown` as `Unmanaged` is not
        // splitting them at all. Disabling something that was never there is a safe no-op, and
        // the read-back at the end of the disable flow is the only way `Unknown` becomes
        // known.
        DesiredArtifact::Disabled { .. } => !matches!(
            applied,
            Some(AppliedArtifactState::Disabled | AppliedArtifactState::Unmanaged)
        ),
    }
}

fn retirement_actions(
    desired: &NodeDesiredState,
    applied: Option<&NodeAppliedState>,
) -> Vec<PlannedAction> {
    let mut actions = Vec::new();
    let mut require_disabled =
        |desired: &DesiredArtifact, observed: Option<&AppliedArtifactState>, action| {
            if matches!(desired, DesiredArtifact::Disabled { .. })
                && !matches!(observed, Some(AppliedArtifactState::Disabled))
            {
                actions.push(action);
            }
        };
    require_disabled(
        &desired.phantun,
        applied.map(|state| &state.phantun),
        PlannedAction::DisablePhantun,
    );
    require_disabled(
        &desired.hy2_port_hop,
        applied.map(|state| &state.hy2_port_hop),
        PlannedAction::DisableHy2PortHop,
    );
    require_disabled(
        &desired.wireguard,
        applied.map(|state| &state.wireguard),
        PlannedAction::DisableWireGuard,
    );
    require_disabled(
        &desired.xray,
        applied.map(|state| &state.xray),
        PlannedAction::DisableXray,
    );
    actions.sort();
    actions
}

fn retirement_artifacts_disabled(applied: &NodeAppliedState) -> bool {
    [
        &applied.phantun,
        &applied.hy2_port_hop,
        &applied.wireguard,
        &applied.xray,
    ]
    .into_iter()
    .all(|state| matches!(state, AppliedArtifactState::Disabled))
}

fn grants_need_action(desired: &DesiredGrants, applied: Option<&AppliedGrantsState>) -> bool {
    match desired {
        DesiredGrants::Unmanaged { .. } | DesiredGrants::Disabled { .. } => false,
        DesiredGrants::Present { .. } => match applied {
            Some(state @ AppliedGrantsState::Present { .. }) => !grants_match(desired, state),
            // `Unmanaged` means the previous deployment did not manage this machine's grants,
            // which is the same ignorance as `Unknown` and must be treated the same: synchronize
            // once. This used to return false, and the consequence was that a machine reporting
            // an unmanaged state once was never updated again, with every grant added afterwards
            // permanently unable to reach it (hit on preview: the state stuck at yesterday, a new
            // user having no effect while the plan page said there was nothing to do). One extra
            // synchronization costs almost nothing: `sync_grants` is idempotent, restarts no
            // xray, and drops no connection.
            Some(AppliedGrantsState::Unmanaged) => true,
            Some(AppliedGrantsState::Disabled | AppliedGrantsState::Unknown) | None => true,
            Some(AppliedGrantsState::Dirty { .. }) => true,
        },
    }
}

/// Whether desired and observed are the same grant set. The test is set equality of
/// `(email, uuid, flow)` per tag, independent of order and independent of level — level is an
/// argument to `adu` and cannot be read back from a machine.
///
/// The planning and judging stages share this one definition; do not write a second.
pub fn grants_match(desired: &DesiredGrants, observed: &AppliedGrantsState) -> bool {
    match desired {
        DesiredGrants::Present { inbounds } => match observed {
            AppliedGrantsState::Present {
                inbounds: observed_inbounds,
            } => desired_grant_keys(inbounds) == observed_grant_keys(observed_inbounds),
            _ => false,
        },
        DesiredGrants::Disabled { .. } => matches!(observed, AppliedGrantsState::Disabled),
        DesiredGrants::Unmanaged { .. } => matches!(observed, AppliedGrantsState::Unmanaged),
    }
}

type GrantKeys = BTreeMap<String, BTreeSet<(String, String, Option<String>)>>;

fn desired_grant_keys(inbounds: &[GrantInbound]) -> GrantKeys {
    inbounds
        .iter()
        .map(|inbound| {
            let clients = inbound
                .clients
                .iter()
                .map(|client| {
                    (
                        client.email.clone(),
                        client.uuid.clone(),
                        client.flow.clone(),
                    )
                })
                .collect();
            (inbound.tag.clone(), clients)
        })
        .collect()
}

fn observed_grant_keys(inbounds: &[ObservedInbound]) -> GrantKeys {
    inbounds
        .iter()
        .map(|inbound| {
            let clients = inbound
                .clients
                .iter()
                .map(|client| {
                    (
                        client.email.clone(),
                        client.uuid.clone(),
                        client.flow.clone(),
                    )
                })
                .collect();
            (inbound.tag.clone(), clients)
        })
        .collect()
}

/// Waves rest on cost, not on an action's name. Disabling costs every connection on the machine
/// and every link through it — provided something was running there. Where the baseline is
/// `Unknown` or there is no record at all, nothing was ever placed on that machine, disabling
/// drops no connection, and what that convergence round actually does is read the state clearly.
/// Counted as destructive, every newly enrolled machine would drag out an exclusive wave awaiting
/// confirmation, while enrollment is meant to complete in one.
fn is_disruptive(
    action: &PlannedAction,
    applied: Option<&NodeAppliedState>,
    desired: &NodeDesiredState,
) -> bool {
    match action {
        // Not every config change costs the machine its connections. Where the difference
        // is one a running xray can be told about, the agent installs it without a restart
        // and nothing drops — so waving and asking an operator to confirm would be charging
        // for a cost that is not paid. Where the control plane cannot prove what the machine
        // is running, or the difference reaches something with no runtime expression, the
        // answer stays the old one.
        //
        // Erring is not symmetric. Saying "destructive" about a change that turns out to be
        // installable costs one confirmation click; saying "harmless" about one that turns
        // out to need a restart breaks a promise on live traffic. Hence every uncertainty
        // resolving to true.
        PlannedAction::ApplyXray => !can_apply_without_restart(applied, desired),
        PlannedAction::DisableXray => was_ours_and_running(applied.map(|state| &state.xray)),
        PlannedAction::DisableWireGuard => {
            was_ours_and_running(applied.map(|state| &state.wireguard))
        }
        // Starting or stopping phantun rebuilds that link's transport layer and drops connections
        // as stopping xray does — but it counts as destructive only where it really was running,
        // and a first installation drops nothing.
        PlannedAction::ApplyPhantun => was_ours_and_running(applied.map(|state| &state.phantun)),
        PlannedAction::ApplyHy2PortHop | PlannedAction::DisableHy2PortHop => {
            was_ours_and_running(applied.map(|state| &state.hy2_port_hop))
        }
        PlannedAction::DisablePhantun => was_ours_and_running(applied.map(|state| &state.phantun)),
        PlannedAction::ApplyWireGuard | PlannedAction::SyncGrants => false,
    }
}

/// Whether this machine's xray change is one the running process can be told about.
///
/// Both halves have to be present and have to agree. The text the control plane believes is
/// running is only believable if its digest is the one the machine reported — a machine that
/// drifted, failed its last release, or was never in it is one whose current config the
/// control plane does not know, and guessing there is exactly the "the control plane
/// believes it knows what a machine looks like" failure this system exists to avoid.
fn can_apply_without_restart(
    applied: Option<&NodeAppliedState>,
    desired: &NodeDesiredState,
) -> bool {
    let Some(applied) = applied else {
        return false;
    };
    let (Some(running), AppliedArtifactState::Present { sha256 }) =
        (applied.running_xray.as_deref(), &applied.xray)
    else {
        return false;
    };
    let DesiredArtifact::Present { content, .. } = &desired.xray else {
        return false;
    };
    if sha256_hex(running.as_bytes()) != *sha256 {
        return false;
    }
    hotswap::hot_swap(running, content).is_some()
}

fn was_ours_and_running(applied: Option<&AppliedArtifactState>) -> bool {
    matches!(
        applied,
        Some(AppliedArtifactState::Present { .. } | AppliedArtifactState::Dirty { .. })
    )
}

fn assign_waves(targets: &mut [PlannedTarget]) {
    let xray_apply_targets = targets
        .iter()
        .enumerate()
        .filter_map(|(index, target)| {
            (target.actions.contains(&PlannedAction::ApplyXray)
                && !target.actions.contains(&PlannedAction::DisableXray))
            .then_some(index)
        })
        .collect::<Vec<_>>();

    if let Some((canary, rest)) = xray_apply_targets.split_first() {
        targets[*canary].wave = 1;
        for index in rest {
            targets[*index].wave = 2;
        }
    }

    let high_risk_start_wave = match xray_apply_targets.len() {
        0 => 1,
        1 => 2,
        _ => 3,
    };
    // Only a disable that really drops something claims an exclusive wave; disabling a service
    // that never ran travels in an ordinary one (see `is_disruptive`).
    let high_risk_targets = targets
        .iter()
        .enumerate()
        .filter_map(|(index, target)| {
            (target.disruptive
                && (target.actions.contains(&PlannedAction::DisableWireGuard)
                    || target.actions.contains(&PlannedAction::DisableXray)))
            .then_some(index)
        })
        .collect::<Vec<_>>();

    for (offset, index) in high_risk_targets.into_iter().enumerate() {
        targets[index].wave = high_risk_start_wave + offset as u32;
    }
}

/// Counts the plan's targets the way every reader of a plan needs them counted.
///
/// Public because the store answers the same question about plans it has already stored and about
/// the redacted view it hands a tenant admin. Three copies of these five counts existed; a
/// disagreement between them is an operator being told a different number of machines will change
/// than the number that changes.
pub fn summarize_targets(targets: &[PlannedTarget]) -> PlanSummary {
    let total_targets = targets.len();
    let skipped_targets = targets
        .iter()
        .filter(|target| target.status == PlannedTargetStatus::Skipped)
        .count();
    let changed_targets = total_targets - skipped_targets;
    let disruptive_targets = targets.iter().filter(|target| target.disruptive).count();
    let max_wave = targets.iter().map(|target| target.wave).max().unwrap_or(0);

    PlanSummary {
        total_targets,
        changed_targets,
        skipped_targets,
        disruptive_targets,
        max_wave,
    }
}
