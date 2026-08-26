use crate::{
    diagnostic::{summarize_diagnostics, Diagnostic, DiagnosticSummary},
    ir::{
        hops::compile_hops,
        routing::{compile_app, AppIr},
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
        Ok(node::project_node(&self.system, &self.apps, node_id))
    }

    pub fn project_user(&self, tenant: &str, user_id: &str) -> Result<UserPlan, PublishBlocked> {
        self.ensure_publishable()?;
        Ok(user::project_user(&self.apps, tenant, user_id))
    }

    /// Which chains this machine, as a chain head, must probe. Like `project_node`
    /// it requires the model to compile — for a model that does not compile, there
    /// is no meaningful notion of its expected exits.
    pub fn project_probe(&self, node_id: &str) -> Result<ProbePlan, PublishBlocked> {
        self.ensure_publishable()?;
        Ok(probe::project_probe(&self.apps, node_id))
    }
}

pub fn compile(snapshot: &ModelSnapshot) -> CompileOutput {
    let mut diagnostics = Vec::new();

    validate_model_snapshot(snapshot, &mut diagnostics);
    let system = compile_system(snapshot, &mut diagnostics);
    validate_system(&system, &mut diagnostics);

    let mut source_apps = snapshot.apps.iter().collect::<Vec<_>>();
    source_apps.sort_by(|a, b| a.id.cmp(&b.id));

    let mut apps = Vec::new();
    for app in source_apps {
        let app_ir = compile_app(snapshot, app, &mut diagnostics);
        let app_ir = compile_hops(app_ir, &system, &mut diagnostics);
        validate_app(&system, &app_ir, &mut diagnostics);
        apps.push(app_ir);
    }

    validate_app_set(&apps, &mut diagnostics);
    let summary = summarize_diagnostics(&diagnostics);

    CompileOutput {
        system,
        apps,
        diagnostics,
        summary,
    }
}
