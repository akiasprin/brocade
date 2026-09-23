use std::collections::BTreeSet;

use crate::{
    diagnostic::{summarize_diagnostics, Diagnostic, DiagnosticSummary},
    ir::{
        hops::compile_hops_with_targets,
        routing::{
            compile_app_with_listener_roots, compile_app_with_listener_roots_and,
            reachable_listener_roots_across_apps, AppIr, RoutingChainCompile, RoutingChainInput,
        },
        system::{compile_system, SystemIr},
        validate::{validate_app, validate_app_set, validate_model_snapshot, validate_system},
    },
    model::ModelSnapshot,
    physical::{
        node::{self, NodePlan},
        probe::{self, ProbePlan},
        user::{self, UserPlan},
    },
};

/// The global half of one compilation.  It is intentionally an explicit value rather than a
/// process cache: `brocade-core` stays a pure compiler, while callers may memoize this immutable
/// stage by content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilePrelude {
    system: SystemIr,
    listener_roots: BTreeSet<(String, String)>,
    diagnostics: Vec<Diagnostic>,
}

/// One project's routing IR before relay endpoints are resolved against the complete project set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingAppCompile {
    app: AppIr,
    diagnostics: Vec<Diagnostic>,
}

/// One project's final IR plus diagnostics produced while resolving and validating that project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalAppCompile {
    app: AppIr,
    diagnostics: Vec<Diagnostic>,
}

impl CompilePrelude {
    pub fn system(&self) -> &SystemIr {
        &self.system
    }

    pub fn listener_roots(&self) -> &BTreeSet<(String, String)> {
        &self.listener_roots
    }
}

impl RoutingAppCompile {
    pub fn app(&self) -> &AppIr {
        &self.app
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl FinalAppCompile {
    pub fn app(&self) -> &AppIr {
        &self.app
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileOutput {
    system: SystemIr,
    apps: Vec<AppIr>,
    pub diagnostics: Vec<Diagnostic>,
    pub summary: DiagnosticSummary,
}

/// The intermediate representation even when compilation found errors.
///
/// This view exists for diagnostics such as the console's compile inspector. It is deliberately
/// named after the fact that its contents may not be publishable: artifacts and runtime work
/// lists must use the gated `project_*` methods on [`CompileOutput`] instead.
#[derive(Debug, Clone, Copy)]
pub struct UnpublishableView<'a> {
    pub system: &'a SystemIr,
    pub apps: &'a [AppIr],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishBlocked {
    pub summary: DiagnosticSummary,
    pub diagnostics: Vec<Diagnostic>,
}

impl CompileOutput {
    /// Revision is provenance, not compiler input. Content-addressed callers compile a canonical
    /// revision-zero snapshot and rebind the resulting immutable IR to the requested revision.
    pub fn with_revision(mut self, revision: u64) -> Self {
        self.system.revision = revision;
        for app in &mut self.apps {
            app.revision = revision;
        }
        self
    }

    pub fn unpublishable_view(&self) -> UnpublishableView<'_> {
        UnpublishableView {
            system: &self.system,
            apps: &self.apps,
        }
    }

    pub fn can_publish(&self) -> bool {
        self.summary.can_publish
    }

    pub fn ensure_publishable(&self) -> Result<(), PublishBlocked> {
        if self.can_publish() {
            Ok(())
        } else {
            Err(PublishBlocked {
                summary: self.summary,
                diagnostics: self.diagnostics.clone(),
            })
        }
    }

    pub fn project_node(&self, node_id: &str) -> Result<NodePlan, PublishBlocked> {
        self.ensure_publishable()?;
        let apps = node::scope_node_apps(&self.apps, node_id);
        Ok(node::project_node(&self.system, &apps, node_id))
    }

    pub fn project_user(&self, tenant: &str, user_id: &str) -> Result<UserPlan, PublishBlocked> {
        self.ensure_publishable()?;
        let apps = user::scope_user_apps(&self.apps, tenant, user_id);
        Ok(user::project_user(&apps, tenant, user_id))
    }

    /// Which chains this machine, as a chain head, must probe. Like `project_node`
    /// it requires the model to compile — for a model that does not compile, there
    /// is no meaningful notion of its expected exits.
    pub fn project_probe(&self, node_id: &str) -> Result<ProbePlan, PublishBlocked> {
        self.ensure_publishable()?;
        let apps = probe::scope_probe_apps(&self.apps, node_id);
        Ok(probe::project_probe(&apps, node_id))
    }
}

pub fn compile(snapshot: &ModelSnapshot) -> CompileOutput {
    let prelude = compile_prelude(snapshot);
    let routing_apps = snapshot
        .apps
        .iter()
        .map(|app| compile_routing_app(snapshot, app, prelude.listener_roots()))
        .collect::<Vec<_>>();
    let target_steps = routing_apps
        .iter()
        .flat_map(|app| app.app().steps.iter().cloned())
        .collect::<Vec<_>>();
    let apps = routing_apps
        .iter()
        .cloned()
        .map(|app| finish_app_compile(app, &target_steps, prelude.system()))
        .collect::<Vec<_>>();
    finish_compile(prelude, &routing_apps, apps)
}

/// Compile the model-wide system layer and listener reachability index.
pub fn compile_prelude(snapshot: &ModelSnapshot) -> CompilePrelude {
    let mut diagnostics = Vec::new();

    validate_model_snapshot(snapshot, &mut diagnostics);
    let system = compile_system(snapshot, &mut diagnostics);
    validate_system(&system, &mut diagnostics);
    let listener_roots = reachable_listener_roots_across_apps(snapshot);

    CompilePrelude {
        system,
        listener_roots,
        diagnostics,
    }
}

/// Compile one project without resolving its relay endpoints.  This is the first independently
/// memoizable application stage; cross-project listener roots are an explicit input.
pub fn compile_routing_app(
    snapshot: &ModelSnapshot,
    app: &crate::model::AppView,
    listener_roots: &BTreeSet<(String, String)>,
) -> RoutingAppCompile {
    let mut diagnostics = Vec::new();
    let app =
        compile_app_with_listener_roots(snapshot, app, Some(listener_roots), &mut diagnostics);
    RoutingAppCompile { app, diagnostics }
}

/// Variant of [`compile_routing_app`] whose chain units may be supplied by a caller-owned cache.
/// The callback receives a complete immutable input, so a cached value never depends on ambient
/// compiler state.
pub fn compile_routing_app_with<F>(
    snapshot: &ModelSnapshot,
    app: &crate::model::AppView,
    listener_roots: &BTreeSet<(String, String)>,
    compile_chain: F,
) -> RoutingAppCompile
where
    F: FnMut(&RoutingChainInput) -> RoutingChainCompile,
{
    let mut diagnostics = Vec::new();
    let app = compile_app_with_listener_roots_and(
        snapshot,
        app,
        Some(listener_roots),
        &mut diagnostics,
        compile_chain,
    );
    RoutingAppCompile { app, diagnostics }
}

/// Resolve one project's hops against the complete routing-step index, then run project-local
/// validation.  Keeping this separate prevents a cached routing stage from hiding a changed
/// cross-project listener contract.
pub fn finish_app_compile(
    routing: RoutingAppCompile,
    target_steps: &[crate::ir::routing::Step],
    system: &SystemIr,
) -> FinalAppCompile {
    let mut diagnostics = Vec::new();
    let app = compile_hops_with_targets(routing.app, target_steps, system, &mut diagnostics);
    validate_app(system, &app, &mut diagnostics);
    FinalAppCompile { app, diagnostics }
}

/// Assemble cached or freshly compiled stages in the same diagnostic order as the historical
/// monolithic compiler: global, every routing project, every final project, then global checks.
pub fn finish_compile(
    prelude: CompilePrelude,
    routing_apps: &[RoutingAppCompile],
    apps: Vec<FinalAppCompile>,
) -> CompileOutput {
    let mut diagnostics = prelude.diagnostics;
    for app in routing_apps {
        diagnostics.extend(app.diagnostics.iter().cloned());
    }
    let apps = apps
        .into_iter()
        .map(|app| {
            diagnostics.extend(app.diagnostics);
            app.app
        })
        .collect::<Vec<_>>();

    validate_app_set(&apps, &mut diagnostics);
    let summary = summarize_diagnostics(&diagnostics);

    CompileOutput {
        system: prelude.system,
        apps,
        diagnostics,
        summary,
    }
}
