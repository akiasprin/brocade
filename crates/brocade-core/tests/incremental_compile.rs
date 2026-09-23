mod fixture;

use brocade_core::{
    compile::{
        compile, compile_prelude, compile_routing_app_with, finish_app_compile, finish_compile,
    },
    physical::{
        node::{project_node, scope_node_apps},
        probe::{project_probe, scope_probe_apps},
        user::{project_user, scope_user_apps},
    },
};

#[test]
fn chain_stages_assemble_to_the_monolithic_result() {
    let snapshot = fixture::demo_snapshot();
    let expected = compile(&snapshot);
    let prelude = compile_prelude(&snapshot);
    let routing = snapshot
        .apps
        .iter()
        .map(|app| {
            compile_routing_app_with(&snapshot, app, prelude.listener_roots(), |input| {
                input.compile()
            })
        })
        .collect::<Vec<_>>();
    let target_steps = routing
        .iter()
        .flat_map(|app| app.app().steps.iter().cloned())
        .collect::<Vec<_>>();
    let apps = routing
        .iter()
        .cloned()
        .map(|app| finish_app_compile(app, &target_steps, prelude.system()))
        .collect();
    let actual = finish_compile(prelude, &routing, apps);

    assert_eq!(actual, expected);
}

#[test]
fn target_scopes_preserve_every_demo_projection() {
    let snapshot = fixture::demo_snapshot();
    let output = compile(&snapshot);
    assert!(output.can_publish(), "{:#?}", output.diagnostics);
    let view = output.unpublishable_view();

    for node in &snapshot.nodes {
        let full = project_node(view.system, view.apps, &node.id);
        let scoped = scope_node_apps(view.apps, &node.id);
        assert_eq!(
            full,
            project_node(view.system, &scoped, &node.id),
            "node {}",
            node.id
        );

        let full = project_probe(view.apps, &node.id);
        let scoped = scope_probe_apps(view.apps, &node.id);
        assert_eq!(full, project_probe(&scoped, &node.id), "probe {}", node.id);
    }

    for user in &snapshot.users {
        let full = project_user(view.apps, &user.tenant, &user.id);
        let scoped = scope_user_apps(view.apps, &user.tenant, &user.id);
        assert_eq!(
            full,
            project_user(&scoped, &user.tenant, &user.id),
            "user {}/{}",
            user.tenant,
            user.id
        );
    }
}
