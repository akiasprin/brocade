use super::*;

const RELAY_LABEL: &str = "chn-a1b2-c3d4@old-node-id";

fn relay_bindings(owner: &str) -> serde_json::Value {
    json!({RELAY_LABEL: {
        "kind": "chain-hop", "tenant_id": "platform.acme", "app_id": "app-main",
        "chain_id": "chn-a1b2-c3d4", "node_id": owner
    }})
}

fn relay_report(
    generation: i64,
    sequence: u64,
    at: i64,
    epoch: &str,
    up: u64,
    down: u64,
) -> UsageReportRequest {
    let mut request = current_usage(generation, sequence, at, epoch, up, down);
    request.counters[0].label = RELAY_LABEL.to_owned();
    request
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_retires_chain_tail_under_its_original_frozen_owner() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let base = usage_report_base_unix();
    let original = install_usage_generation(db.pool(), "n1", relay_bindings("n1"), base - 60).await;
    for (sequence, at, up, down) in [(1, base, 100, 200), (2, base + 30, 150, 300)] {
        db.store
            .record_usage_report(
                "n1",
                relay_report(original, sequence, at, "journal:stable", up, down),
            )
            .await
            .unwrap();
    }
    let retired = install_usage_generation(
        db.pool(),
        "n1",
        json!({RELAY_LABEL: {"kind": "foreign"}}),
        base + 30,
    )
    .await;
    let same_second = db
        .store
        .record_usage_report(
            "n1",
            relay_report(retired, 3, base + 30, "journal:stable", 160, 320),
        )
        .await
        .unwrap();
    assert_eq!(same_second.rejected_counters, 0);
    assert_eq!(same_second.accepted_readings, 0);
    let tail = relay_report(retired, 4, base + 60, "journal:stable", 180, 350);
    let result = db
        .store
        .record_usage_report("n1", tail.clone())
        .await
        .unwrap();
    assert_eq!(result.usage_generation_id, retired);
    assert_eq!(result.accepted_readings, 1);
    assert_eq!(result.inserted_samples, 1);
    assert_eq!(result.rejected_counters, 0);
    let sample: (i64, String, i64, i64, bool) = sqlx::query_as(
        "SELECT generation_id,node_id,uplink_bytes,downlink_bytes,has_gap
         FROM usage_chain_samples ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(sample, (original, "n1".to_owned(), 30, 50, false));
    assert!(
        db.store
            .record_usage_report("n1", tail)
            .await
            .unwrap()
            .duplicate
    );

    let idle = db
        .store
        .record_usage_report(
            "n1",
            relay_report(retired, 5, base + 90, "journal:stable", 180, 350),
        )
        .await
        .unwrap();
    assert_eq!(idle.skipped_counters, 1);
    assert_eq!(idle.rejected_counters, 0);

    // A later topology may remove the label altogether. Raw retention and the new topology
    // must not erase the durable proof that these cumulative bytes belong to the old hop.
    sqlx::query("DELETE FROM usage_readings")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM usage_chain_samples")
        .execute(db.pool())
        .await
        .unwrap();
    let absent = install_usage_generation(db.pool(), "n1", json!({}), base + 90).await;
    let result = db
        .store
        .record_usage_report(
            "n1",
            relay_report(absent, 6, base + 120, "journal:stable", 200, 400),
        )
        .await
        .unwrap();
    assert_eq!(result.accepted_readings, 1);
    assert_eq!(result.rejected_counters, 0);
    let totals: (i64, i64) = sqlx::query_as(
        "SELECT relay_uplink_bytes,relay_downlink_bytes FROM usage_node_rollups WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        totals,
        (100, 200),
        "only growth beyond the original baseline is booked"
    );
    let head_generation: i64 = sqlx::query_scalar(
        "SELECT generation_id FROM usage_counter_heads WHERE node_id='n1' AND label=$1",
    )
    .bind(RELAY_LABEL)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        head_generation, original,
        "tail ownership remains auditable after retention"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_retired_chain_tail_rejects_unproven_namespaces_and_sources() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;
    let base = usage_report_base_unix();
    let original = install_usage_generation(db.pool(), "n1", relay_bindings("n1"), base - 60).await;
    db.store
        .record_usage_report(
            "n1",
            relay_report(original, 1, base, "journal:stable", 100, 200),
        )
        .await
        .unwrap();
    let retired = install_usage_generation(
        db.pool(),
        "n1",
        json!({RELAY_LABEL: {"kind": "foreign"}}),
        base,
    )
    .await;
    for (index, (at, epoch, up, down)) in [
        (base + 30, "journal:stable", 99, 250),
        (base + 31, "journal:stable", 150, 199),
        (base + 32, "journal:other", 150, 250),
        (base - 1, "journal:stable", 150, 250),
    ]
    .into_iter()
    .enumerate()
    {
        let result = db
            .store
            .record_usage_report(
                "n1",
                relay_report(retired, index as u64 + 2, at, epoch, up, down),
            )
            .await
            .unwrap();
        assert_eq!(result.rejected_counters, 1);
        assert_eq!(result.accepted_readings, 0);
    }
    let foreign = install_usage_generation(
        db.pool(),
        "n2",
        json!({RELAY_LABEL: {"kind": "foreign"}}),
        base,
    )
    .await;
    let forged = db
        .store
        .record_usage_report(
            "n2",
            relay_report(foreign, 1, base + 40, "journal:stable", 150, 250),
        )
        .await
        .unwrap();
    assert_eq!(forged.rejected_counters, 1);
    let head: (i64, i64, i64) = sqlx::query_as(
        "SELECT generation_id,uplink_bytes,downlink_bytes FROM usage_counter_heads
         WHERE node_id='n1' AND label=$1",
    )
    .bind(RELAY_LABEL)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(head, (original, 100, 200));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_migrated_reverse_owner_uses_ledger_identity_and_isolates_unresolved_items() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;
    let base = usage_report_base_unix();
    let mut bindings = alice_usage_bindings();
    bindings[RELAY_LABEL] = relay_bindings("n2")[RELAY_LABEL].clone();
    let generation = install_usage_generation(db.pool(), "n1", bindings.clone(), base - 60).await;
    let report = |generation, sequence, at, up, down| {
        let mut request = current_usage(generation, sequence, at, "journal:stable", up, down);
        request.counters.push(UsageCounter {
            label: RELAY_LABEL.to_owned(),
            uplink_bytes: up,
            downlink_bytes: down,
        });
        request
    };
    for (sequence, at, up, down) in [(1, base, 100, 200), (2, base + 30, 150, 300)] {
        db.store
            .record_usage_report("n1", report(generation, sequence, at, up, down))
            .await
            .unwrap();
    }

    // Reproduce a manual ID migration which moved FK-backed history but left the JSON
    // attribution pointing at an ID that no longer exists. The label itself stays unchanged.
    let mut stale = bindings.clone();
    stale[RELAY_LABEL]["node_id"] = json!("old-node-id");
    sqlx::query("UPDATE usage_generations SET bindings=$2 WHERE id=$1")
        .bind(generation)
        .bind(&stale)
        .execute(db.pool())
        .await
        .unwrap();
    let migrated = db
        .store
        .record_usage_report("n1", report(generation, 3, base + 60, 180, 350))
        .await
        .unwrap();
    assert_eq!(migrated.accepted_readings, 2);
    assert_eq!(migrated.rejected_counters, 0);
    let owner: String =
        sqlx::query_scalar("SELECT node_id FROM usage_chain_samples ORDER BY id DESC LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        owner, "n2",
        "reverse usage must not fall back to the reporter n1"
    );
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT bindings FROM usage_generations WHERE id=$1")
            .bind(generation)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored, stale, "ingestion does not rewrite frozen history");

    // An old ID may already have been reused. The exact ledger head still identifies the
    // migrated bridge; merely finding a live node with the JSON's stale ID is insufficient.
    let mut reused = stale.clone();
    reused[RELAY_LABEL]["node_id"] = json!("n1");
    sqlx::query("UPDATE usage_generations SET bindings=$2 WHERE id=$1")
        .bind(generation)
        .bind(&reused)
        .execute(db.pool())
        .await
        .unwrap();
    let reused_result = db
        .store
        .record_usage_report("n1", report(generation, 4, base + 75, 190, 375))
        .await
        .unwrap();
    assert_eq!(reused_result.rejected_counters, 0);
    let owner: String =
        sqlx::query_scalar("SELECT node_id FROM usage_chain_samples ORDER BY id DESC LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(owner, "n2");
    sqlx::query("UPDATE usage_generations SET bindings=$2 WHERE id=$1")
        .bind(generation)
        .bind(&stale)
        .execute(db.pool())
        .await
        .unwrap();

    // If all ownership evidence has expired, refuse only this hop. Its durable baseline
    // remains available for a later known-good generation to recover the unbooked delta.
    sqlx::query("DELETE FROM usage_chain_samples")
        .execute(db.pool())
        .await
        .unwrap();
    let unresolved_request = report(generation, 5, base + 90, 200, 400);
    let unresolved = db
        .store
        .record_usage_report("n1", unresolved_request.clone())
        .await
        .unwrap();
    assert_eq!(unresolved.accepted_readings, 1);
    assert_eq!(unresolved.rejected_counters, 1);
    assert!(
        db.store
            .record_usage_report("n1", unresolved_request)
            .await
            .unwrap()
            .duplicate
    );
    let head: (i64, i64) = sqlx::query_as("SELECT uplink_bytes,downlink_bytes FROM usage_counter_heads WHERE node_id='n1' AND label=$1")
        .bind(RELAY_LABEL).fetch_one(db.pool()).await.unwrap();
    assert_eq!(head, (190, 375));
    let recovered = install_usage_generation(db.pool(), "n1", bindings, base + 90).await;
    let result = db
        .store
        .record_usage_report("n1", report(recovered, 6, base + 120, 220, 450))
        .await
        .unwrap();
    assert_eq!(result.rejected_counters, 0);
    let totals: (i64, i64) = sqlx::query_as(
        "SELECT relay_uplink_bytes,relay_downlink_bytes FROM usage_node_rollups WHERE node_id='n2'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        totals,
        (120, 250),
        "recovery includes the temporarily rejected interval"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_migrated_owner_rejects_ambiguous_ledger_evidence() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;
    let base = usage_report_base_unix();
    let generation =
        install_usage_generation(db.pool(), "n1", relay_bindings("n2"), base - 60).await;
    for (sequence, at, up, down) in [(1, base, 100, 200), (2, base + 30, 150, 300)] {
        db.store
            .record_usage_report(
                "n1",
                relay_report(generation, sequence, at, "journal:stable", up, down),
            )
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO usage_chain_samples (
            window_start,window_end,node_id,tenant_id,app_id,chain_id,hop_label,
            uplink_bytes,downlink_bytes,has_gap,generation_id
         ) SELECT window_start,window_end,'n1',tenant_id,app_id,chain_id,hop_label,
                  uplink_bytes,downlink_bytes,has_gap,generation_id
           FROM usage_chain_samples WHERE generation_id=$1",
    )
    .bind(generation)
    .execute(db.pool())
    .await
    .unwrap();
    let result = db
        .store
        .record_usage_report(
            "n1",
            relay_report(generation, 3, base + 60, "journal:stable", 180, 350),
        )
        .await
        .unwrap();
    assert_eq!(result.rejected_counters, 1);
    assert_eq!(result.accepted_readings, 0);
    let head: (i64, i64) = sqlx::query_as(
        "SELECT uplink_bytes,downlink_bytes FROM usage_counter_heads WHERE node_id='n1' AND label=$1",
    )
    .bind(RELAY_LABEL)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        head,
        (150, 300),
        "ambiguous evidence cannot replace the known head"
    );
}
