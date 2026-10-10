//! Human-readable source provenance for embedded artifacts.
//!
//! The release identity remains the digest of the actual bytes. This module answers a different
//! question: which repository change last touched an embedded component's own source closure?
//! Using the repository HEAD would advance the displayed Agent or Xray version for a Console,
//! frontend, or README commit even when that embedded component is byte-for-byte unchanged.

use std::{path::Path, process::Command};

/// Repository paths containing the local source-crate closure of `brocade-agent`.
///
/// Keep this to source crates rather than broad workspace inputs such as `Cargo.lock`: a Console-
/// only dependency update can rewrite the shared lockfile without changing the Agent. Toolchain
/// and dependency-resolution changes remain visible in the artifact digest, which is the
/// authoritative machine identity.
pub const AGENT_SOURCE_PATHS: &[&str] = &[
    "crates/brocade-agent",
    "crates/brocade-probe",
    "crates/brocade-deployment",
    "crates/brocade-core",
];

/// The vendored Xray fork is one source tree, independent of the Console and Agent crates.
pub const XRAY_SOURCE_PATHS: &[&str] = &["components/xray-core"];

pub fn agent_source_revision(workspace: &Path) -> String {
    describe_paths(workspace, AGENT_SOURCE_PATHS)
}

pub fn xray_source_revision(workspace: &Path) -> String {
    describe_paths(workspace, XRAY_SOURCE_PATHS)
}

/// The newest commit that changed `paths`, annotated only when one of those paths is dirty.
///
/// `git status` is deliberately scoped to the same closure as `git rev-list`: unrelated working
/// tree changes must not turn a clean Agent source revision into `-dirty`.
pub fn describe_paths(workspace: &Path, paths: &[&str]) -> String {
    let mut revision = Command::new("git");
    revision
        .arg("-C")
        .arg(workspace)
        .args(["rev-list", "-1", "HEAD", "--"])
        .args(paths);
    let revision = output(revision)
        .filter(|value| !value.is_empty())
        .filter(|value| valid_revision(value));

    let mut status = Command::new("git");
    status
        .arg("-C")
        .arg(workspace)
        .args(["status", "--porcelain", "--untracked-files=normal", "--"])
        .args(paths);
    let Some(status) = output(status) else {
        return "unknown".to_owned();
    };
    let dirty = !status.is_empty();

    match (revision, dirty) {
        (Some(revision), true) => format!("{revision}-dirty"),
        (Some(revision), false) => revision,
        (None, true) => "unknown-dirty".to_owned(),
        (None, false) => "unknown".to_owned(),
    }
}

fn output(mut command: Command) -> Option<String> {
    command
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn valid_revision(value: &str) -> bool {
    (40..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
