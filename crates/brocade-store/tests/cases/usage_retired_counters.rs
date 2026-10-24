use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_report_skips_only_proven_unchanged_retired_counters() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let base = usage_report_base_unix();
    let hop_label = "chn-a1b2-c3d4@n1";
    let mut bindings = alice_usage_bindings();
    bindings[hop_label] = json!({
        "kind": "chain-hop", "tenant_id": "platform.acme", "app_id": "app-main",
        "chain_id": "chn-a1b2-c3d4", "node_id": "n1"
    });
    let owned = install_usage_generation(db.pool(), "n1", bindings.clone(), base - 60).await;
    let report = |generation, sequence, read_at, epoch: &str, up, down| {
        let mut request = current_usage(generation, sequence, read_at, epoch, up, down);
        request.counters.push(UsageCounter {
            label: hop_label.to_owned(),
            uplink_bytes: up,
            downlink_bytes: down,
        });
        request
    };
    for (sequence, read_at, up, down) in [(1, base, 100, 200), (2, base + 30, 150, 300)] {
        let result = db
            .store
            .record_usage_report(
                "n1",
                report(owned, sequence, read_at, "journal:stable", up, down),
            )
            .await
            .unwrap();
        assert_eq!(result.accepted_readings, 2);
        assert_eq!(result.rejected_counters, 0);
    }
    let head_before: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(h) ORDER BY label) FROM usage_counter_heads h WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    // The new topology no longer carries either label, but Xray retains both absolute counters.
    for binding in bindings.as_object_mut().unwrap().values_mut() {
        *binding = json!({"kind": "foreign"});
    }
    let retired = install_usage_generation(db.pool(), "n1", bindings, base + 30).await;
    for (sequence, read_at) in [(3, base + 30), (4, base + 60)] {
        let request = report(retired, sequence, read_at, "journal:stable", 150, 300);
        let result = db
            .store
            .record_usage_report("n1", request.clone())
            .await
            .unwrap();
        assert_eq!(result.skipped_counters, 2);
        assert_eq!(result.rejected_counters, 0);
        assert_eq!(result.accepted_readings, 0);
        assert_eq!(result.inserted_samples, 0);
        let duplicate = db.store.record_usage_report("n1", request).await.unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.skipped_counters, 2);
        assert_eq!(duplicate.rejected_counters, 0);
    }

    // Either direction moving, an unproven epoch or an older timestamp still signals a problem.
    for (index, (read_at, epoch, up, down)) in [
        (base + 70, "journal:stable", 151, 300),
        (base + 71, "journal:stable", 150, 301),
        (base + 72, "journal:stable", 149, 300),
        (base + 73, "journal:stable", 150, 299),
        (base + 74, "journal:other", 150, 300),
        (base + 29, "journal:stable", 150, 300),
    ]
    .into_iter()
    .enumerate()
    {
        let result = db
            .store
            .record_usage_report(
                "n1",
                report(retired, index as u64 + 5, read_at, epoch, up, down),
            )
            .await
            .unwrap();
        assert_eq!(result.rejected_counters, 2);
        assert_eq!(result.skipped_counters, 0);
        assert_eq!(result.accepted_readings, 0);
        assert_eq!(result.inserted_samples, 0);
    }
    let head_after: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(h) ORDER BY label) FROM usage_counter_heads h WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        head_after, head_before,
        "neither skipping nor rejecting advances a billing head"
    );
    let readings: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_readings")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        readings, 4,
        "foreign counters do not become accepted raw readings"
    );

    // A later authorized generation still differences against the last valid cumulative value.
    let mut restored = alice_usage_bindings();
    restored[hop_label] = json!({
        "kind": "chain-hop", "tenant_id": "platform.acme", "app_id": "app-main",
        "chain_id": "chn-a1b2-c3d4", "node_id": "n1"
    });
    let restored = install_usage_generation(db.pool(), "n1", restored, base + 80).await;
    let result = db
        .store
        .record_usage_report(
            "n1",
            report(restored, 11, base + 90, "journal:stable", 180, 350),
        )
        .await
        .unwrap();
    assert_eq!(result.rejected_counters, 0);
    assert_eq!(result.accepted_readings, 2);
    assert_eq!(result.inserted_samples, 2);
    let totals: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT sum(user_uplink_bytes)::bigint,sum(user_downlink_bytes)::bigint,
                sum(relay_uplink_bytes)::bigint,sum(relay_downlink_bytes)::bigint
         FROM usage_node_rollups WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        totals,
        (80, 150, 80, 150),
        "historical totals are never charged twice"
    );
}
