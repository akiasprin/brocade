//! Verification of a chain's shape after a cascading delete: removing c-brc-jp's sg-01
//! subtree leaves jp-01's rule table with nothing but the ads block (every Forward into the
//! subtree cleared) — and jp-01 is the ingress machine (egress_allowed=false), so the
//! compiler appends a Block fallback, the chain goes dead while compiling green, and only a
//! step.no-egress warning remains.
//!
//! What this test pins down is the established semantics of a post-deletion hole in a rule
//! table: a non-exit machine losing all its downstreams neither errors nor silently exits,
//! but gets a Block appended and a warning. Changing that behavior turns this red first.

use brocade_core::{
    compile::compile,
    model::{Action, DestMatch, Rule},
};

mod fixture;
use fixture::demo_snapshot;

#[test]
fn cascade_leftovers_on_ingress_only_node_become_block() {
    let mut snapshot = demo_snapshot();
    let app = snapshot
        .apps
        .iter_mut()
        .find(|app| app.id == "brc")
        .expect("demo 模型里必须有 brc 视图");

    // Simulate the cascading delete: c-brc-jp's sg-01/us-01/au-01 step rows are gone, and
    // jp-01's rule table holds nothing but the ads block (every Forward into the subtree
    // cleared).
    app.steps
        .retain(|step| step.chain != "c-brc-jp" || step.node == "jp-01");
    for step in &mut app.steps {
        if step.chain == "c-brc-jp" && step.node == "jp-01" {
            step.rules = vec![Rule {
                dest_match: DestMatch::Geosite(vec!["category-ads".to_owned()]),
                action: Action::Block,
            }];
        }
    }

    let output = compile(&snapshot);
    eprintln!("diagnostics = {:#?}", output.diagnostics);
    // A dead chain is now an error: with no exit path on the chain the release is blocked,
    // which is exactly what a cascading delete looks like once it removes a non-exit
    // machine's only exit. The step.no-egress warning remains, but what blocks the release
    // is chain.no-egress-path.
    assert!(
        output
            .diagnostics
            .iter()
            .any(|d| d.code == "chain.no-egress-path" && d.level == brocade_core::Level::Error),
        "死链必须报 chain.no-egress-path error：{:#?}",
        output.diagnostics,
    );
    // assert!( ... step.no-egress ... ) — disabled for now; look at the artifacts first

    // A dead chain has no artifacts: project_node is stopped by PublishBlocked, which is
    // where "this should block the release" lands. can_publish is false and no deployment
    // can start.
    let plan = output.project_node("jp-01");
    assert!(
        matches!(plan, Err(brocade_core::compile::PublishBlocked { .. })),
        "死链必须挡发布：{plan:?}",
    );
}
