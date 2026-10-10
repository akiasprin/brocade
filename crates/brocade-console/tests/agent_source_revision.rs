#[path = "../build/source_revision.rs"]
mod source_revision;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

struct TestRepo(PathBuf);

impl TestRepo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "brocade-agent-source-revision-{}-{}",
            std::process::id(),
            NEXT_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "-q"]);
        Self(path)
    }

    fn commit(&self, message: &str) -> String {
        git(&self.0, &["add", "."]);
        git(
            &self.0,
            &[
                "-c",
                "user.name=Brocade Test",
                "-c",
                "user.email=test@invalid.example",
                "commit",
                "-q",
                "-m",
                message,
            ],
        );
        git(&self.0, &["rev-parse", "HEAD"])
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn unrelated_commits_and_dirty_files_do_not_advance_component_revisions() {
    let repo = TestRepo::new();
    let agent = repo.0.join("crates/brocade-agent/src");
    fs::create_dir_all(&agent).unwrap();
    fs::write(agent.join("main.rs"), "fn main() {}\n").unwrap();
    let agent_commit = repo.commit("agent");

    let xray = repo.0.join("components/xray-core/main");
    fs::create_dir_all(&xray).unwrap();
    fs::write(xray.join("main.go"), "package main\n").unwrap();
    let xray_commit = repo.commit("xray");

    let frontend = repo.0.join("frontend/src");
    fs::create_dir_all(&frontend).unwrap();
    fs::write(frontend.join("main.ts"), "export {};\n").unwrap();
    let head = repo.commit("frontend");
    assert_ne!(head, agent_commit);
    assert_eq!(
        source_revision::agent_source_revision(&repo.0),
        agent_commit
    );
    assert_eq!(source_revision::xray_source_revision(&repo.0), xray_commit);

    fs::write(frontend.join("main.ts"), "export const changed = true;\n").unwrap();
    assert_eq!(
        source_revision::describe_paths(&repo.0, &["crates/brocade-agent"]),
        agent_commit
    );
    assert_eq!(source_revision::xray_source_revision(&repo.0), xray_commit);

    fs::write(xray.join("main.go"), "package main\n\nfunc main() {}\n").unwrap();
    assert_eq!(
        source_revision::xray_source_revision(&repo.0),
        format!("{xray_commit}-dirty")
    );
}

#[test]
fn related_commits_advance_and_only_related_dirt_marks_the_revision() {
    let repo = TestRepo::new();
    let agent = repo.0.join("crates/brocade-agent/src");
    fs::create_dir_all(&agent).unwrap();
    fs::write(agent.join("main.rs"), "fn main() {}\n").unwrap();
    let first = repo.commit("first agent");

    fs::write(agent.join("main.rs"), "fn main() { println!(\"new\"); }\n").unwrap();
    assert_eq!(
        source_revision::describe_paths(&repo.0, &["crates/brocade-agent"]),
        format!("{first}-dirty")
    );

    let second = repo.commit("second agent");
    assert_ne!(first, second);
    assert_eq!(
        source_revision::describe_paths(&repo.0, &["crates/brocade-agent"]),
        second
    );
}
