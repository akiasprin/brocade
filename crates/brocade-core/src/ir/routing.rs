use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    net::IpAddr,
};

use serde::{Deserialize, Serialize};

use crate::{
    diagnostic::Diagnostic,
    model::{
        self, grant_label, Action, Dns, DomainStrategy, FrontStrategy, HopDial, HopWire,
        IngressIdentity, IngressWires, NodeConnection, Projection,
    },
};

/// A rule's matching half, which is the model-layer type verbatim.
///
/// This layer does not rewrite matches, so a second type has no place here. The IR
/// appends only a trailing `Any`; it never changes one match into another. An
/// identically shaped second type buys only a field-by-field cloning conversion, plus
/// the rule that adding a match kind means remembering to edit two places.
///
/// The `Action` upstream follows the same reasoning and has always used the
/// model-layer type directly.
pub use crate::model::DestMatch;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// The application-layer IR, the counterpart to `SystemIr` (the system layer) — the
/// system layer is the mesh of trusted nodes, this layer is the business running on
/// top of it.
///
/// Note that the `App` here and the `app_id` below are not the same word:
/// - the App in `AppIr` is the application layer, one per snapshot;
/// - the app in `app_id` is a project (the console's term), several of which can
///   coexist in one snapshot.
///
/// So "the application-layer IR holds a project id" is coherent, if a mouthful.
/// Straightening it out means renaming the type (`AppIr` → `RoutingIr`; the file is
/// already called routing.rs), which is a large change reaching into the schema — to
/// be done the next time this concept itself has to move.
pub struct AppIr {
    pub revision: u64,
    /// Which project to compile; `None` means all of them.
    pub app_id: Option<String>,
    pub tenants: Vec<String>,
    pub nodes: Vec<AppNode>,
    pub users: Vec<User>,
    /// Tenant-owned external proxy resources visible to the compiler. They are terminal routing
    /// targets, not nodes, and therefore never participate in `hops` or chain membership. Tenant
    /// scope validation decides which of them this project's chains may actually reference.
    pub external_outbounds: Vec<model::ExternalOutbound>,
    pub chains: Vec<Chain>,
    pub ingresses: Vec<Ingress>,
    pub fronts: Vec<Front>,
    pub steps: Vec<Step>,
    /// Additional roots kept alive because a reachable rule table references their listener.
    ///
    /// This is compiler bookkeeping, not wire output. In particular, a reference may originate
    /// in another project, so inspecting only this AppIr's rules cannot reconstruct the set.
    #[serde(skip)]
    pub listener_roots: BTreeSet<(String, String)>,
    pub grants: Vec<Grant>,
    pub hops: Vec<super::hops::Hop>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppNode {
    pub id: String,
    pub tenant: String,
    pub name: String,
    /// Which Agent certificate directory this node's TLS listeners use. This belongs in the
    /// application projection as well as SystemIr: a pure ingress/egress node is intentionally
    /// absent from the overlay system layer but still owns Xray listeners.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_group_id: Option<String>,
    #[serde(default)]
    pub certificate_track: Option<crate::model::CertificateTrack>,
    pub public_ipv4: Option<String>,
    pub public_ipv6: Option<String>,
    pub public_ipv4_nat: bool,
    pub public_ipv6_nat: bool,
    pub egress_allowed: bool,
    pub api_port: Option<u16>,
    pub dns: Dns,
    pub domain_strategy: DomainStrategy,
    /// Machine-owned DNS policies. Repeated in each project's node view because node
    /// artifacts are projected from the application IR; the source snapshot remains canonical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_dns: Vec<model::NodeEgressDnsPolicy>,
    /// Verbatim from the model, still unmerged — the global defaults live on
    /// `SystemIr::settings` and the two are combined in `physical/node.rs`, where both
    /// halves are in scope.
    pub connection: NodeConnection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub tenant: String,
    pub uuid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chain {
    pub id: String,
    pub app_id: Option<String>,
    pub tenant: String,
    pub name: String,
    pub subscription_country: Option<String>,
    /// The node where this chain begins, derived once from its sole ingress.
    ///
    /// `None` keeps a malformed chain without an ingress representable long enough for
    /// `chain.no-ingress` to diagnose it. More than one ingress is rejected at the model
    /// boundary by `chain.multi-ingress`.
    pub root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ingress {
    pub id: String,
    pub app_id: Option<String>,
    pub tenant: String,
    pub chain: String,
    pub node: String,
    pub bind: IpAddr,
    pub port: u16,
    pub front: Option<String>,
    pub sniff: bool,
    pub identity: IngressIdentity,
    pub anytls_identity: Option<IngressIdentity>,
    pub wires: IngressWires,
    /// The name on the certificate held by the machine this ingress listens on, resolved here so
    /// that the three readers of it — the artifact, the subscription and the probe — all take it
    /// from one place. `None` for a machine with none issued, which only a shape that presents
    /// its own certificate has any reason to care about.
    pub certificate_name: Option<String>,
    /// Both runtime-slot names. `certificate_name` selects new client projections; listeners use
    /// this set so changing that selection never makes the other slot's saved SNI invalid.
    pub certificate_names: Vec<String>,
    /// What this entrance refuses to carry. Copied from the model unchanged — the compiler decides
    /// what each flag becomes, not whether it applies.
    pub guard: crate::model::IngressGuard,
    /// Verbatim from the model. Only subscription rendering reads it
    /// (`physical/user.rs`); neither routing nor the artifacts do — it describes where
    /// to tell a user to dial, not how traffic travels.
    pub projection: Projection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Front {
    pub id: String,
    pub app_id: Option<String>,
    pub tenant: String,
    pub name: String,
    pub via: Vec<String>,
    pub external_via: Vec<String>,
    pub strategy: FrontStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub app_id: Option<String>,
    pub chain: String,
    pub node: String,
    pub accept: Option<Accept>,
    /// This chain's relay inbound on this machine. `None` means the chain accepts no
    /// relay here.
    ///
    /// It carries a private key, as `accept` does — a `Step` is projected into this
    /// machine's own artifacts (`physical/node.rs`) and never flows into anyone else's
    /// hands. The dialer's half lives in `Hop.security`.
    pub hop_in: Option<HopIn>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HopIn {
    pub port: u16,
    pub security: HopWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accept {
    pub uuid: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub dest_match: DestMatch,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub app_id: Option<String>,
    pub tenant: String,
    pub user: String,
    pub ingress: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressRoute {
    pub tag: String,
    pub send_through: Option<IpAddr>,
}

/// The exact owned input of one chain-routing compilation.  Keeping this value independent from
/// the rest of its project lets the control plane content-address and reuse a large project's
/// unchanged chains without putting cache state inside the pure compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutingChainInput {
    app_id: String,
    chain: model::Chain,
    root: Option<String>,
    nodes: Vec<model::Node>,
    steps: Vec<model::Step>,
    reused_listener_nodes: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingChainCompile {
    chain: Chain,
    steps: Vec<Step>,
    diagnostics: Vec<Diagnostic>,
}

impl RoutingChainInput {
    pub fn compile(&self) -> RoutingChainCompile {
        let mut diagnostics = Vec::new();
        let chain = Chain {
            id: self.chain.id.clone(),
            app_id: Some(self.app_id.clone()),
            tenant: self.chain.tenant.clone(),
            name: self.chain.name.clone(),
            subscription_country: self.chain.subscription_country.clone(),
            root: self.root.clone(),
        };
        let mut steps = Vec::new();
        compile_chain_steps(self, &mut diagnostics, &mut steps);
        RoutingChainCompile {
            chain,
            steps,
            diagnostics,
        }
    }
}

pub fn compile_app(
    doc: &model::ModelSnapshot,
    app: &model::AppView,
    diagnostics: &mut Vec<Diagnostic>,
) -> AppIr {
    compile_app_with_listener_roots(doc, app, None, diagnostics)
}

pub(crate) fn compile_app_with_listener_roots(
    doc: &model::ModelSnapshot,
    app: &model::AppView,
    listener_roots: Option<&BTreeSet<(String, String)>>,
    diagnostics: &mut Vec<Diagnostic>,
) -> AppIr {
    compile_app_with_listener_roots_and(doc, app, listener_roots, diagnostics, |input| {
        input.compile()
    })
}

pub(crate) fn compile_app_with_listener_roots_and<F>(
    doc: &model::ModelSnapshot,
    app: &model::AppView,
    listener_roots: Option<&BTreeSet<(String, String)>>,
    diagnostics: &mut Vec<Diagnostic>,
    mut compile_chain: F,
) -> AppIr
where
    F: FnMut(&RoutingChainInput) -> RoutingChainCompile,
{
    let chain_by_id = app
        .chains
        .iter()
        .map(|chain| (chain.id.as_str(), chain))
        .collect::<HashMap<_, _>>();
    let node_by_id = doc
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();

    let mut ir = AppIr {
        revision: doc.revision,
        app_id: Some(app.id.clone()),
        tenants: Vec::new(),
        // A decommissioned machine stays out of the application layer too. Removing it
        // from the backbone alone is not enough: `xray_plan` looks it up through the
        // AppIr node table, so left here it receives a skeleton xray.json holding only
        // the API inbound — which is Present, not Disabled, so the agent keeps an xray
        // running while decommissioning means tearing it down (see the note on
        // `retired` in model.rs).
        nodes: doc
            .nodes
            .iter()
            .filter(|node| !node.retired)
            .map(|node| AppNode {
                id: node.id.clone(),
                tenant: node.tenant.clone(),
                name: node.name.clone(),
                certificate_group_id: node.certificate_group_id.clone(),
                certificate_track: node.certificate_track,
                public_ipv4: node.public_ipv4.clone(),
                public_ipv6: node.public_ipv6.clone(),
                public_ipv4_nat: node.public_ipv4_nat,
                public_ipv6_nat: node.public_ipv6_nat,
                egress_allowed: node.egress_allowed,
                api_port: node.api_port,
                dns: node.dns.clone(),
                domain_strategy: node.domain_strategy,
                egress_dns: doc
                    .node_egress_dns
                    .iter()
                    .filter(|policy| policy.node == node.id)
                    .cloned()
                    .collect(),
                connection: node.connection,
            })
            .collect(),
        users: doc
            .users
            .iter()
            .map(|user| User {
                id: user.id.clone(),
                tenant: user.tenant.clone(),
                uuid: user.uuid.clone(),
            })
            .collect(),
        external_outbounds: doc.external_outbounds.to_vec(),
        chains: Vec::new(),
        ingresses: Vec::new(),
        fronts: Vec::new(),
        steps: Vec::new(),
        listener_roots: BTreeSet::new(),
        grants: Vec::new(),
        hops: Vec::new(),
    };

    let retired = doc
        .nodes
        .iter()
        .filter(|node| node.retired)
        .map(|node| node.id.as_str())
        .collect::<BTreeSet<_>>();
    let chain_roots = chain_roots(app, diagnostics);
    let disabled_chains = disabled_chains(app, &retired, &chain_roots);
    let live_ingresses = app
        .ingresses
        .iter()
        .filter(|ingress| is_live_ingress(ingress, &node_by_id, &disabled_chains))
        .map(|ingress| ingress.id.as_str())
        .collect::<BTreeSet<_>>();

    ir.fronts = app
        .fronts
        .iter()
        .map(|front| Front {
            id: front.id.clone(),
            app_id: Some(app.id.clone()),
            tenant: front.tenant.clone(),
            name: front.name.clone(),
            // A via pointing at an ingress that is already gone is dropped along with
            // it: kept, the validation layer reports `front.unknown-via` — "the front
            // group points at a nonexistent ingress" — which is a consequence of the
            // decommissioning rather than a mistyped group, and reporting it only
            // blocks the decommissioning from shipping. The subscription layer already
            // computes group membership from the ingresses a person actually has
            // (`front_groups` in `physical/user.rs`), and one fewer member is
            // something it has always handled.
            via: front
                .via
                .iter()
                .filter(|via| live_ingresses.contains(via.as_str()))
                .cloned()
                .collect(),
            external_via: front.external_via.clone(),
            strategy: front.strategy,
        })
        .collect();

    ir.ingresses = app
        .ingresses
        .iter()
        .filter(|ingress| is_live_ingress(ingress, &node_by_id, &disabled_chains))
        .map(|ingress| {
            let tenant = chain_by_id
                .get(ingress.chain.as_str())
                .map(|chain| chain.tenant.clone())
                .unwrap_or_else(|| "platform".to_owned());

            // Empty is the durable model's compact representation of "follow global". Resolve
            // it before the IR fans out to artifacts, probes and subscriptions so every consumer
            // sees the same concrete server scheme. An ingress override remains untouched.
            let mut wires = ingress.wires.clone();
            if let Some(anytls) = wires.anytls_mut() {
                if anytls.padding_scheme.is_empty() {
                    anytls.padding_scheme = doc.settings.anytls_padding_scheme.clone();
                }
            }

            Ingress {
                id: ingress.id.clone(),
                app_id: Some(app.id.clone()),
                tenant,
                chain: ingress.chain.clone(),
                node: ingress.node.clone(),
                bind: ingress.bind,
                port: ingress.port,
                front: ingress.front.clone(),
                // Front membership is a client subscription concern. It must not change the
                // listener or any other machine artifact.
                sniff: true,
                identity: ingress.identity.clone(),
                anytls_identity: ingress.anytls_identity.clone(),
                wires,
                certificate_name: node_by_id
                    .get(ingress.node.as_str())
                    .and_then(|node| node.certificate_name.clone()),
                certificate_names: node_by_id
                    .get(ingress.node.as_str())
                    .map(|node| node.certificate_names.clone())
                    .unwrap_or_default(),
                projection: ingress.projection.clone(),
                guard: ingress.guard,
            }
        })
        .collect();

    let local_listener_roots;
    let reused_listeners = match listener_roots {
        Some(listener_roots) => listener_roots,
        None => {
            local_listener_roots = reachable_reused_listeners(app, &chain_roots, &disabled_chains);
            &local_listener_roots
        }
    };
    ir.listener_roots = reused_listeners
        .iter()
        .filter(|(chain, _)| app.chains.iter().any(|candidate| candidate.id == *chain))
        .cloned()
        .collect();
    for chain in &app.chains {
        if disabled_chains.contains(chain.id.as_str()) {
            continue;
        }
        let root = chain_roots
            .get(chain.id.as_str())
            .map(|ingress| ingress.node.as_str());

        let input = routing_chain_input(doc, app, chain, root, reused_listeners);
        let compiled = compile_chain(&input);
        ir.chains.push(compiled.chain);
        ir.steps.extend(compiled.steps);
        diagnostics.extend(compiled.diagnostics);
    }

    compile_grants(doc, app, &ir.ingresses, diagnostics, &mut ir.grants);
    collect_tenants(&mut ir);
    sort_ir(&mut ir);
    ir
}

fn routing_chain_input(
    doc: &model::ModelSnapshot,
    app: &model::AppView,
    chain: &model::Chain,
    root: Option<&str>,
    reused_listeners: &BTreeSet<(String, String)>,
) -> RoutingChainInput {
    let steps = app
        .steps
        .iter()
        .filter(|step| step.chain == chain.id)
        .cloned()
        .collect::<Vec<_>>();
    let mut node_ids = root.into_iter().map(str::to_owned).collect::<BTreeSet<_>>();
    for step in &steps {
        node_ids.insert(step.node.clone());
        for rule in &step.rules {
            if let Action::Forward { to, .. } = &rule.action {
                node_ids.insert(to.clone());
            }
        }
    }
    let nodes = doc
        .nodes
        .iter()
        .filter(|node| node_ids.contains(&node.id))
        .cloned()
        .collect();
    let reused_listener_nodes = reused_listeners
        .iter()
        .filter(|(owner_chain, _)| owner_chain == &chain.id)
        .map(|(_, node)| node.clone())
        .collect();
    RoutingChainInput {
        app_id: app.id.clone(),
        chain: chain.clone(),
        root: root.map(str::to_owned),
        nodes,
        steps,
        reused_listener_nodes,
    }
}

/// Chains with a decommissioned node on the trunk (the range reachable from the head
/// by expanding Forward): the whole chain is disabled.
///
/// Once a node is decommissioned the chain shuts down with it — no Chain IR is
/// generated, no steps are compiled, and the relay ports and forwarding rules the
/// other nodes hold on this chain vanish along with them. The same reasoning as for
/// ingresses: what a decommissioned node carries shuts down automatically rather than
/// blocking anyone with a "still in service" complaint. The head is the machine
/// hosting the ingress; chain membership has no other declaration and follows wherever
/// the rules Forward. A chain with no ingress has no head, compiles to nothing, and is
/// reported as `chain.no-ingress` by the validation layer.
fn disabled_chains<'a>(
    app: &'a model::AppView,
    retired: &BTreeSet<&str>,
    chain_roots: &HashMap<&'a str, &'a model::Ingress>,
) -> BTreeSet<&'a str> {
    app.chains
        .iter()
        .filter(|chain| {
            chain_roots.get(chain.id.as_str()).is_some_and(|root| {
                chain_members(app, chain.id.as_str(), root.node.as_str())
                    .iter()
                    .any(|node| retired.contains(node.as_str()))
            })
        })
        .map(|chain| chain.id.as_str())
        .collect()
}

/// Resolve every chain's head once, before any compilation decision consumes it.
///
/// The head is a machine, not an ingress: it is where the walk along Forwards begins. So several
/// ingresses on one chain are ordinary — a Hysteria 2 door and a VLESS door, or two ports with
/// different flow settings — as long as they all sit on the same machine, which leaves the walk
/// one starting point. Ingresses on *different* machines are what has no answer, and that is what
/// `chain.multi-ingress` blocks.
///
/// The head is still picked deterministically in that broken case, so console output and
/// validation do not change with the source array's order while publication is blocked.
fn chain_roots<'a>(
    app: &'a model::AppView,
    diagnostics: &mut Vec<Diagnostic>,
) -> HashMap<&'a str, &'a model::Ingress> {
    let roots = selected_chain_roots(app);
    let mut ingresses_by_chain = BTreeMap::<&str, Vec<(&str, &str)>>::new();
    for ingress in &app.ingresses {
        ingresses_by_chain
            .entry(ingress.chain.as_str())
            .or_default()
            .push((ingress.node.as_str(), ingress.id.as_str()));
    }

    let known_chains = app
        .chains
        .iter()
        .map(|chain| chain.id.as_str())
        .collect::<BTreeSet<_>>();
    for (chain, mut ingresses) in ingresses_by_chain {
        let nodes = ingresses
            .iter()
            .map(|(node, _)| *node)
            .collect::<BTreeSet<_>>();
        if !known_chains.contains(chain) || nodes.len() <= 1 {
            continue;
        }
        ingresses.sort_unstable();
        diagnostics.push(Diagnostic::error(
            "chain.multi-ingress",
            chain,
            format!(
                "项目 {} 的链 {chain} 的接入面落在多台机器上：{}；一条链的接入面必须都在同一台机器上",
                app.id,
                ingresses
                    .iter()
                    .map(|(node, id)| format!("{id}@{node}"))
                    .collect::<Vec<_>>()
                    .join("、")
            ),
        ));
    }
    roots
}

/// Pick the same deterministic chain head used by [`chain_roots`] without emitting diagnostics.
/// The snapshot-wide listener walk needs all heads before any one project is compiled; keeping the
/// selection in one helper prevents a malformed multi-ingress chain from getting two different
/// roots in those phases.
fn selected_chain_roots(app: &model::AppView) -> HashMap<&str, &model::Ingress> {
    let mut roots = HashMap::<&str, &model::Ingress>::new();
    for ingress in &app.ingresses {
        match roots.get_mut(ingress.chain.as_str()) {
            Some(root)
                if (ingress.id.as_str(), ingress.node.as_str())
                    < (root.id.as_str(), root.node.as_str()) =>
            {
                *root = ingress;
            }
            Some(_) => {}
            None => {
                roots.insert(ingress.chain.as_str(), ingress);
            }
        }
    }
    roots
}

/// Whether this ingress belongs in the IR.
///
/// Two tests, neither dispensable:
///
/// 1. Its machine is decommissioned. Once a node is decommissioned both wg0 and xray
///    are torn down (it enters neither layer), and an ingress left in the IR has
///    `grant_sync_plan` generating ingress updates for an already Disabled machine,
///    plus an unconnectable entry in users' subscriptions.
///
/// 2. The chain is disabled, even where the head machine itself is perfectly fine. A
///    disabled chain compiles to no steps, so the head carries not one forwarding rule
///    and not one outbound (`outbounds` in `xray.json` is simply empty and xray will
///    not even start), while the ingress still holds its port open and `grant_sync`
///    still pushes users into it — connecting can only reach a black hole.
///
/// The first test alone misses "what was decommissioned is downstream of the chain":
/// there the chain is disabled and the ingress remains, and the symptom is the
/// validation layer blocking the release with `ingress.no-chain` while the operator
/// cannot get the machine decommissioned.
fn is_live_ingress(
    ingress: &model::Ingress,
    node_by_id: &HashMap<&str, &model::Node>,
    disabled_chains: &BTreeSet<&str>,
) -> bool {
    !node_by_id
        .get(ingress.node.as_str())
        .is_some_and(|node| node.retired)
        && !disabled_chains.contains(ingress.chain.as_str())
}

/// Breadth-first expansion from the head along the Forwards in each node's rule table
/// for this chain, yielding the chain's member set. Membership has no declaration
/// table: whoever the rules point at is a hop on the chain. Both the
/// decommission-disable test and ingress reachability rest on it.
fn chain_members(app: &model::AppView, chain_id: &str, root: &str) -> BTreeSet<String> {
    chain_members_from_steps(&app.steps, chain_id, root)
}

fn chain_members_from_steps(steps: &[model::Step], chain_id: &str, root: &str) -> BTreeSet<String> {
    let mut members = BTreeSet::new();
    let mut queue = VecDeque::from([root.to_owned()]);
    while let Some(node_id) = queue.pop_front() {
        if !members.insert(node_id.clone()) {
            continue;
        }
        for step in steps_at_source(steps, chain_id, &node_id) {
            for rule in &step.rules {
                if let model::Action::Forward { to, .. } = &rule.action {
                    queue.push_back(to.clone());
                }
            }
        }
    }
    members
}

/// Listener roots reached by live user traffic.
///
/// Scanning every stored Step would let an already-orphaned source row keep an unrelated target
/// subtree alive and turn the source's existing `step.unreachable` warning into a publish-blocking
/// target error. Start at each live ingress instead and follow both owned and referenced edges.
/// The walk also discovers references nested inside a referenced subtree.
fn reachable_reused_listeners(
    app: &model::AppView,
    roots: &HashMap<&str, &model::Ingress>,
    disabled_chains: &BTreeSet<&str>,
) -> BTreeSet<(String, String)> {
    let known_chains = app
        .chains
        .iter()
        .map(|chain| chain.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut initial = app
        .chains
        .iter()
        .filter(|chain| !disabled_chains.contains(chain.id.as_str()))
        .filter_map(|chain| {
            roots
                .get(chain.id.as_str())
                .map(|root| (chain.id.clone(), root.node.clone()))
        })
        .collect::<Vec<_>>();
    initial.sort();

    let mut queue = VecDeque::from(initial);
    let mut seen = BTreeSet::new();
    let mut reused = BTreeSet::new();
    while let Some((chain, node)) = queue.pop_front() {
        if !seen.insert((chain.clone(), node.clone())) {
            continue;
        }
        for step in steps_at(app, &chain, &node) {
            for rule in &step.rules {
                match &rule.action {
                    Action::Forward { to, .. } => queue.push_back((chain.clone(), to.clone())),
                    Action::ReuseListener { listener, .. } => {
                        reused.insert((listener.chain.clone(), listener.node.clone()));
                        if known_chains.contains(listener.chain.as_str())
                            && !disabled_chains.contains(listener.chain.as_str())
                        {
                            queue.push_back((listener.chain.clone(), listener.node.clone()));
                        }
                    }
                    Action::Egress { .. } | Action::Proxy { .. } | Action::Block => {}
                }
            }
        }
    }
    reused
}

/// Listener roots reached from every live ingress in the snapshot.
///
/// Chain ids are durable, globally unique store keys, so a `ListenerRef { chain, node }` remains
/// unambiguous when its owner is in another project. The walk must nevertheless happen before
/// compiling projects one by one: otherwise the target project's compiler cannot see an incoming
/// reference from the source project and drops an otherwise orphaned listener subtree.
pub(crate) fn reachable_listener_roots_across_apps(
    doc: &model::ModelSnapshot,
) -> BTreeSet<(String, String)> {
    let retired = doc
        .nodes
        .iter()
        .filter(|node| node.retired)
        .map(|node| node.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut owners = BTreeMap::<&str, &model::AppView>::new();
    let mut roots = BTreeMap::<&str, &model::Ingress>::new();
    let mut disabled = BTreeSet::<&str>::new();

    for app in &doc.apps {
        let app_roots = selected_chain_roots(app);
        let app_disabled = disabled_chains(app, &retired, &app_roots);
        for chain in &app.chains {
            // PostgreSQL makes chain ids globally unique. Keep the first occurrence deterministic
            // for malformed in-memory snapshots; validation still reports the duplicate ids.
            owners.entry(chain.id.as_str()).or_insert(app);
            if app_disabled.contains(chain.id.as_str()) {
                disabled.insert(chain.id.as_str());
            }
            if let Some(root) = app_roots.get(chain.id.as_str()) {
                roots.entry(chain.id.as_str()).or_insert(*root);
            }
        }
    }

    let mut initial = roots
        .iter()
        .filter(|(chain, _)| !disabled.contains(**chain))
        .map(|(chain, ingress)| ((*chain).to_owned(), ingress.node.clone()))
        .collect::<Vec<_>>();
    initial.sort();

    let mut queue = VecDeque::from(initial);
    let mut seen = BTreeSet::new();
    let mut reused = BTreeSet::new();
    while let Some((chain, node)) = queue.pop_front() {
        if !seen.insert((chain.clone(), node.clone())) {
            continue;
        }
        let Some(app) = owners.get(chain.as_str()).copied() else {
            continue;
        };
        for step in steps_at(app, &chain, &node) {
            for rule in &step.rules {
                match &rule.action {
                    Action::Forward { to, .. } => queue.push_back((chain.clone(), to.clone())),
                    Action::ReuseListener { listener, .. } => {
                        reused.insert((listener.chain.clone(), listener.node.clone()));
                        if owners.contains_key(listener.chain.as_str())
                            && !disabled.contains(listener.chain.as_str())
                        {
                            queue.push_back((listener.chain.clone(), listener.node.clone()));
                        }
                    }
                    Action::Egress { .. } | Action::Proxy { .. } | Action::Block => {}
                }
            }
        }
    }
    reused
}

/// All source fragments for one chain node, in model order.
///
/// A fragment contributes rules to the node's one ordered rule table. Keeping this lookup shared
/// by membership discovery and IR construction prevents a Forward seen by one phase from being
/// silently absent in the other.
fn steps_at<'a>(app: &'a model::AppView, chain_id: &str, node_id: &str) -> Vec<&'a model::Step> {
    steps_at_source(&app.steps, chain_id, node_id)
}

fn steps_at_source<'a>(
    steps: &'a [model::Step],
    chain_id: &str,
    node_id: &str,
) -> Vec<&'a model::Step> {
    steps
        .iter()
        .filter(|step| step.chain == chain_id && step.node == node_id)
        .collect()
}

fn merge_step_field<'a, T: Clone + PartialEq + 'a>(
    values: impl IntoIterator<Item = Option<&'a T>>,
    code: &'static str,
    location: &str,
    field_name: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<T> {
    let mut merged = None::<T>;
    let mut conflicted = false;
    for value in values.into_iter().flatten() {
        match &merged {
            Some(previous) if previous != value => conflicted = true,
            Some(_) => {}
            None => merged = Some(value.clone()),
        }
    }
    if conflicted {
        diagnostics.push(Diagnostic::error(
            code,
            location,
            format!("同一节点的多个 Step 写了不同的{field_name}"),
        ));
        None
    } else {
        merged
    }
}

fn compile_chain_steps(
    input: &RoutingChainInput,
    diagnostics: &mut Vec<Diagnostic>,
    out: &mut Vec<Step>,
) {
    let Some(root) = input.root.as_deref() else {
        return;
    };
    let chain = &input.chain;
    let source_steps = &input.steps;
    let nodes = &input.nodes;
    let reused_listener_nodes = &input.reused_listener_nodes;

    // A referenced listener is another root of this chain's stored rule forest. It remains owned
    // by this chain, but need not also be reachable from the chain's user ingress: removing the
    // owner's incoming edge must not make a subtree still used elsewhere disappear from the
    // compiled artifacts. Stable ordering keeps the output independent of rule insertion order.
    let primary_members = chain_members_from_steps(source_steps, &chain.id, root);
    let mut roots = vec![root.to_owned()];
    roots.extend(reused_listener_nodes.iter().cloned());
    roots.sort();
    roots.dedup();
    if let Some(index) = roots.iter().position(|node| node == root) {
        roots.swap(0, index);
    }
    let mut queue = VecDeque::from(roots.clone());
    let mut seen = roots.into_iter().collect::<BTreeSet<_>>();
    let mut made = BTreeSet::new();
    let mut steps = Vec::new();
    // Whether any member had a Block fallback appended by the compiler because of a
    // hole in its rule table (the step.no-egress case). The dead-chain error counts
    // only this — a chain that explicitly writes any→Block is a user deciding to block
    // everything, not a hole.
    let mut padded_block = false;

    while let Some(node_id) = queue.pop_front() {
        if made.contains(&node_id) {
            continue;
        }
        let node = match nodes.iter().find(|node| node.id == node_id) {
            Some(node) => node,
            None => {
                diagnostics.push(Diagnostic::error(
                    "chain.unknown-node",
                    &chain.id,
                    format!("链上的 {node_id} 不存在"),
                ));
                continue;
            }
        };

        let sources = steps_at_source(source_steps, &chain.id, &node_id);
        let source_rules = sources
            .iter()
            .flat_map(|step| step.rules.iter().cloned())
            .collect::<Vec<_>>();
        let mut rules = source_rules
            .into_iter()
            .map(|rule| Rule {
                dest_match: rule.dest_match,
                action: rule.action,
            })
            .collect::<Vec<_>>();
        let at = format!("{}/{}", chain.id, node_id);
        let source_accept = merge_step_field(
            sources.iter().map(|step| step.accept.as_ref()),
            "step.accept-conflict",
            &at,
            "接受凭据",
            diagnostics,
        );
        let source_hop_in = merge_step_field(
            sources.iter().map(|step| step.hop_in.as_ref()),
            "step.hop-in-conflict",
            &at,
            "中转 inbound",
            diagnostics,
        );

        let padded = append_default_rule(node, chain, &node_id, diagnostics, &mut rules);
        if primary_members.contains(&node_id) && matches!(padded, Some(Action::Block)) {
            padded_block = true;
        }

        // The root node needs neither: it receives user traffic (through the ingress),
        // accepts no relays, and configuring a relay port there would open a listening
        // port out of thin air that nobody dials. Reverse access is the exception, and
        // only half an exception — there the downstream connects to the upstream, so
        // the port must be open while the head acts as upstream or the tunnel has
        // nowhere to attach (the symptom being `relay.no-hop-in` pointing at the head).
        // `accept` is still cleared: it is the key others dial me with, which a head
        // still should not have, and a downstream connecting in uses its own
        // credential, placed into this port's clients by `physical/node.rs`.
        let reverse_upstream = rules.iter().any(|rule| {
            matches!(
                &rule.action,
                Action::Forward {
                    dial: HopDial::Reverse(_),
                    ..
                }
            )
        });
        let reused = reused_listener_nodes.contains(&node_id);
        let (accept, hop_in) = if node_id == *root && !reused {
            (None, reverse_upstream.then_some(source_hop_in).flatten())
        } else {
            (source_accept, source_hop_in)
        };

        for rule in &rules {
            if let Action::Forward { to, .. } = &rule.action {
                if seen.insert(to.clone()) {
                    queue.push_back(to.clone());
                }
            }
        }

        made.insert(node_id.clone());
        steps.push(Step {
            id: format!("s-{}-{node_id}", chain.id),
            app_id: Some(input.app_id.clone()),
            chain: chain.id.clone(),
            node: node_id,
            accept: accept.map(|accept| Accept {
                uuid: accept.uuid,
                label: accept.label,
            }),
            hop_in: hop_in.map(|hop_in| HopIn {
                port: hop_in.port,
                security: hop_in.security,
            }),
            rules,
        });
    }

    let mut unreachable = BTreeMap::<&str, usize>::new();
    for source in source_steps.iter().filter(|source| {
        source.chain == chain.id
            && !made.contains(&source.node)
            && nodes.iter().any(|node| node.id == source.node)
    }) {
        *unreachable.entry(source.node.as_str()).or_default() += source.rules.len();
    }
    for (node, rule_count) in unreachable {
        diagnostics.push(Diagnostic::warn(
            "step.unreachable",
            format!("{}/{}", chain.id, node),
            format!(
                "{node} 从入口节点不可达（没有规则 Forward 指向它），{rule_count} 条规则不会产出"
            ),
        ));
    }

    // Dead-chain test: no exit action anywhere among the members reachable from the
    // head (neither an Egress, nor a reverse tunnel letting the downstream exit on its
    // own) means a dead chain, where all incoming user traffic is Blocked. Deletion
    // triggers it most often — a cascade removes the sole exit and the resulting hole
    // in the rule table has the compiler append a Block fallback. The `step.no-egress`
    // warning says only "this node got a Block appended" and cannot say "the chain no
    // longer has an exit at all", and it is the latter that should block a release.
    if padded_block {
        let alive = steps
            .iter()
            .filter(|step| primary_members.contains(&step.node))
            .any(|step| {
                step.rules.iter().any(|rule| {
                    matches!(
                        &rule.action,
                        Action::Egress { .. }
                            | Action::Proxy { .. }
                            | Action::ReuseListener { .. }
                            | Action::Forward {
                                dial: HopDial::Reverse(_),
                                ..
                            }
                    )
                })
            });
        if !alive {
            diagnostics.push(Diagnostic::error(
                "chain.no-egress-path",
                &chain.id,
                format!("链 {} 上没有任何出网路径（成员全被 Block）", chain.id),
            ));
        }
    }

    out.extend(steps);
}

/// Append the "no rule written = exit here" case. Returns the appended action, or
/// None when nothing was appended (the rule table's last entry is already an any). The
/// dead-chain test uses the return value to recognize a Block that filled a hole — an
/// explicitly written any→Block is a user's decision, a hole-filling Block means the
/// chain may be dead.
fn append_default_rule(
    node: &model::Node,
    chain: &model::Chain,
    node_id: &str,
    diagnostics: &mut Vec<Diagnostic>,
    rules: &mut Vec<Rule>,
) -> Option<Action> {
    if matches!(
        rules.last().map(|rule| &rule.dest_match),
        Some(DestMatch::Any)
    ) {
        return None;
    }

    // A chain's order is expressed entirely by rules, and the compiler appends only
    // the "no rule written = exit here" case — with no trunk declaration there is no
    // "automatically forward to the next hop" to append. A node that wants to go
    // further must write `any → Forward(next)` itself.
    let action = terminal_default(node, chain, node_id, diagnostics);
    rules.push(Rule {
        dest_match: DestMatch::Any,
        action: action.clone(),
    });
    Some(action)
}

fn terminal_default(
    node: &model::Node,
    chain: &model::Chain,
    node_id: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Action {
    if node.egress_allowed {
        Action::Egress { send_through: None }
    } else {
        diagnostics.push(Diagnostic::warn(
            "step.no-egress",
            format!("{}/{}", chain.id, node_id),
            format!("{node_id} 末端禁止出网，补 Block"),
        ));
        Action::Block
    }
}

fn compile_grants(
    doc: &model::ModelSnapshot,
    app: &model::AppView,
    live_ingresses: &[Ingress],
    diagnostics: &mut Vec<Diagnostic>,
    out: &mut Vec<Grant>,
) {
    let mut seen = BTreeSet::new();

    for grant in &app.grants {
        let who = user_key(&grant.tenant, &grant.user);
        let model_ingress = app
            .ingresses
            .iter()
            .find(|ingress| ingress.id == grant.ingress);
        let user = doc
            .users
            .iter()
            .find(|user| user.tenant == grant.tenant && user.id == grant.user);

        if model_ingress.is_none() {
            diagnostics.push(Diagnostic::error(
                "grant.no-ingress",
                format!("{who}->{}", grant.ingress),
                format!("接入面不存在：{}", grant.ingress),
            ));
            continue;
        }
        // The target was valid in the source model but disappeared while compiling — for
        // example because its node retired or a retired downstream disabled the whole chain.
        // The grant belongs to that ingress and leaves with it. Filtering here makes "every IR
        // grant names an IR ingress" true by construction instead of leaving every consumer to
        // rediscover the same rule.
        if !live_ingresses
            .iter()
            .any(|ingress| ingress.id == grant.ingress)
        {
            continue;
        }
        if user.is_none() {
            diagnostics.push(Diagnostic::error(
                "grant.no-user",
                format!("{who}->{}", grant.ingress),
                format!("用户不存在：{who}"),
            ));
            continue;
        }

        let key = format!("{who}|{}", grant.ingress);
        if !seen.insert(key.clone()) {
            diagnostics.push(Diagnostic::error(
                "grant.dup",
                who.clone(),
                format!("重复授权：{who} -> {}", grant.ingress),
            ));
            continue;
        }

        out.push(Grant {
            id: format!("g-{key}"),
            app_id: Some(app.id.clone()),
            tenant: grant.tenant.clone(),
            user: grant.user.clone(),
            ingress: grant.ingress.clone(),
            label: grant_label(&grant.user, &grant.tenant, &grant.ingress),
        });
    }
}

fn collect_tenants(ir: &mut AppIr) {
    let mut tenants = BTreeSet::new();
    tenants.extend(ir.nodes.iter().map(|node| node.tenant.clone()));
    tenants.extend(ir.users.iter().map(|user| user.tenant.clone()));
    tenants.extend(ir.chains.iter().map(|chain| chain.tenant.clone()));
    tenants.extend(ir.ingresses.iter().map(|ingress| ingress.tenant.clone()));
    tenants.extend(ir.fronts.iter().map(|front| front.tenant.clone()));
    tenants.extend(ir.grants.iter().map(|grant| grant.tenant.clone()));
    ir.tenants = tenants.into_iter().collect();
}

fn sort_ir(ir: &mut AppIr) {
    ir.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    // Chain order is semantic: the store materializes this vector from `chains.position`, and
    // console/topology/subscription projections all consume it. The remaining collections are
    // unordered model sets and stay canonicalized by stable ID for machine artifact stability.
    ir.ingresses.sort_by(|a, b| a.id.cmp(&b.id));
    ir.fronts.sort_by(|a, b| a.id.cmp(&b.id));
    ir.steps.sort_by(|a, b| a.id.cmp(&b.id));
    ir.grants.sort_by(|a, b| a.id.cmp(&b.id));
    ir.users
        .sort_by_key(|user| user_key(&user.tenant, &user.id));
}

pub fn egress_tag(send_through: Option<&IpAddr>) -> String {
    match send_through {
        Some(value) => format!("out:egress:{value}"),
        None => "out:egress".to_owned(),
    }
}

fn user_key(tenant: &str, user: &str) -> String {
    format!("{tenant}/{user}")
}
