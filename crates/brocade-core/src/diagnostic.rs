use serde::Serialize;

/// A diagnostic's weight, in descending order of severity.
///
/// `Info` exists because some facts an operator needs to see are not problems. The
/// canonical case is the absence of a link between two NAT'd machines under a full
/// mesh — the mesh put every pair on the table, not the operator, so "this pair
/// cannot connect" is entirely harmless while no chain takes that hop. Reporting it
/// as a warning sends someone to fix a link that may not need to exist at all;
/// where there is a real problem (a chain needs that hop and there is no route),
/// what gets reported is `hop.unreachable`, which is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warn,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub level: Level,
    pub code: &'static str,
    pub location: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DiagnosticSummary {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub can_publish: bool,
}

impl Diagnostic {
    pub fn error(
        code: &'static str,
        location: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level: Level::Error,
            code,
            location: location.into(),
            message: message.into(),
        }
    }

    pub fn warn(
        code: &'static str,
        location: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level: Level::Warn,
            code,
            location: location.into(),
            message: message.into(),
        }
    }

    /// A fact that needs to be seen but is not a problem. See the argument on
    /// `Level::Info`.
    pub fn info(
        code: &'static str,
        location: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level: Level::Info,
            code,
            location: location.into(),
            message: message.into(),
        }
    }
}

pub fn summarize_diagnostics(diagnostics: &[Diagnostic]) -> DiagnosticSummary {
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Error)
        .count();
    let warnings = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Warn)
        .count();
    let infos = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Info)
        .count();

    // `can_publish` looks only at errors — neither info nor warn blocks, and that
    // has not changed.
    DiagnosticSummary {
        errors,
        warnings,
        infos,
        can_publish: errors == 0,
    }
}
