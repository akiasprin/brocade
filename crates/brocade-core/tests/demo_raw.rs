use std::{env, fs, path::PathBuf};

use brocade_core::{
    artifacts::{grants, subscription, wireguard, xray},
    compile::compile,
    format::{ini, json, uri, yaml},
};

mod fixture;
use fixture::{assert_json_file_eq, assert_text_file_eq, demo_snapshot, golden_file};

#[test]
fn demo_raw_configs_match_core_compiler() {
    let snapshot = demo_snapshot();
    let output = compile(&snapshot);
    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);
    // The demo deliberately carries one Pool hop so the concurrency-one artifact path keeps a
    // golden baseline. It is also the one warning the compiler should produce: Pool remains
    // compatible, but new console rules no longer select it because idle Mux.cool workers are not
    // health-checked before reuse.
    assert_eq!(output.summary.warnings, 1, "{:#?}", output.diagnostics);
    assert_eq!(output.diagnostics[0].code, "rule.pool-concurrency-one");
    let dump_dir = env::var_os("BROCADE_DEMO_DUMP_DIR").map(PathBuf::from);
    if let Some(dir) = &dump_dir {
        fs::create_dir_all(dir).unwrap();
    }

    for node_id in ["au-01", "hk-01", "jp-01", "sg-01", "us-01"] {
        let plan = output.project_node(node_id).unwrap();
        let xray_text = json::xray(&xray::build(&plan));
        let wireguard_text = ini::wireguard(&wireguard::build(&plan));
        let grants_batch = grants::build(&plan);
        let grants_text = json::grant_sync_batch(&grants_batch);
        // Only the Rust compiler emits the probe credential (see `physical/node.rs`) and
        // the demo has no counterpart, so it is removed before comparing — kept, this test
        // would be permanently red for a reason having nothing to do with whether the two
        // compilers agree. What is removed is not left unattended:
        // `probe_identity_rides_the_grant_channel` watches it specifically.
        let grants_text_shared = json::grant_sync_batch(&without_probe_clients(&grants_batch));

        if let Some(dir) = &dump_dir {
            fs::write(dir.join(format!("{node_id}.xray.json")), &xray_text).unwrap();
            fs::write(dir.join(format!("{node_id}.wg0.conf")), &wireguard_text).unwrap();
            fs::write(
                dir.join(format!("{node_id}.grants.json-rpc")),
                &grants_text_shared,
            )
            .unwrap();
        }

        assert_json_file_eq(&golden_file(&format!("{node_id}.xray.json")), &xray_text);
        assert_text_file_eq(
            &golden_file(&format!("{node_id}.wg0.conf")),
            &wireguard_text,
        );
        assert_json_file_eq(
            &golden_file(&format!("{node_id}.grants.json-rpc")),
            &grants_text_shared,
        );
        assert!(
            grants_text.contains("probe#") || plan.grant_sync.updates.is_empty(),
            "{node_id} 有入口却没发探测凭据"
        );
    }

    for user_id in ["alice", "bob"] {
        let plan = output.project_user("platform.acme", user_id).unwrap();
        let artifact = subscription::build(&plan);
        let uri_text = uri::subscription(&artifact);
        let clash_text = yaml::clash_subscription(&artifact);

        if let Some(dir) = &dump_dir {
            fs::write(dir.join(format!("{user_id}.uri.txt")), &uri_text).unwrap();
            fs::write(dir.join(format!("{user_id}.clash.yaml")), &clash_text).unwrap();
        }

        assert_text_file_eq(&golden_file(&format!("{user_id}.uri.txt")), &uri_text);
        assert_text_file_eq(&golden_file(&format!("{user_id}.clash.yaml")), &clash_text);
    }
}

/// The grant batch with the probe credential removed. See the note at the call site.
fn without_probe_clients(batch: &grants::GrantSyncBatch) -> grants::GrantSyncBatch {
    grants::GrantSyncBatch {
        node_id: batch.node_id.clone(),
        inbounds: batch
            .inbounds
            .iter()
            .map(|inbound| grants::GrantInboundUpdate {
                inbound_tag: inbound.inbound_tag.clone(),
                clients: inbound
                    .clients
                    .iter()
                    .filter(|client| !brocade_core::model::is_probe_label(&client.label))
                    .cloned()
                    .collect(),
            })
            .collect(),
    }
}
