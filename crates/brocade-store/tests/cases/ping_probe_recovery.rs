use super::*;

async fn setup() -> Option<TestPg> {
    let db = TestPg::start_if_enabled().await?;
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store
        .update_ping_probe_settings(
            &system_admin(),
            PingProbeSettings {
                targets: vec![PingProbeTarget {
                    name: "TCP".to_owned(),
                    kind: PingProbeKind::Tcp,
                    ipv4: Some("192.0.2.1:443".to_owned()),
                    ipv6: None,
                }],
                interval_secs: 10,
                timeout_ms: 420,
            },
        )
        .await
        .unwrap();
    Some(db)
}

fn report(probed_at_unix_secs: i64) -> PingProbeReportRequest {
    PingProbeReportRequest {
        probed_at_unix_secs,
        samples: vec![PingProbeSample {
            target: "tcp://192.0.2.1:443".to_owned(),
            family: Some(PingProbeFamily::Ipv4),
            attempted: true,
            latency_us: Some(12_345),
            skip_reason: None,
        }],
    }
}

async fn now(db: &TestPg) -> i64 {
    sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ping_history_accepts_day_and_multi_day_replay_without_overwriting_latest() {
    let Some(db) = setup().await else { return };
    let now = now(&db).await;
    for timestamp in [now, now - 3 * 86_400, now - 86_400, now - 895] {
        let receipt = db
            .store
            .record_ping_probe("n1", report(timestamp))
            .await
            .unwrap();
        assert_eq!(receipt.accepted_samples, 1);
        assert_eq!(receipt.skipped_samples, 0);
    }
    let duplicate = db
        .store
        .record_ping_probe("n1", report(now - 86_400))
        .await
        .unwrap();
    assert_eq!(duplicate.accepted_samples, 0);
    assert_eq!(duplicate.skipped_samples, 1);
    let timestamps: Vec<i64> = sqlx::query_scalar(
        "SELECT extract(epoch FROM probed_at)::bigint FROM node_ping_probe_samples ORDER BY probed_at",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(timestamps, [now - 3 * 86_400, now - 86_400, now - 895, now]);
    let latest = db
        .store
        .list_latest_node_ping_probes(&system_admin())
        .await
        .unwrap();
    let node = latest
        .nodes
        .iter()
        .find(|node| node.node_id == "n1")
        .unwrap();
    assert_eq!(
        node.targets[0]
            .ipv4
            .as_ref()
            .unwrap()
            .latest
            .as_ref()
            .unwrap()
            .probed_at_unix_secs,
        now
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ping_expired_history_is_acknowledged_using_the_configured_retention() {
    let Some(db) = setup().await else { return };
    let now = now(&db).await;
    // The default seven-day history accepts yesterday's sample, while an operator's one-hour
    // override skips it. Neither case depends on how long this Agent queues its reports.
    let old = db
        .store
        .record_ping_probe("n1", report(now - 8 * 86_400))
        .await
        .unwrap();
    assert_eq!(old.accepted_samples, 0);
    assert_eq!(old.skipped_samples, 1);
    sqlx::query("INSERT INTO admin_operators (id, display_name, role, password_hash) VALUES ('ping-retention', 'Ping retention', 'system-admin', 'test-only')")
        .execute(db.pool()).await.unwrap();
    db.store
        .update_data_retention(
            &AdminContext::system_admin("ping-retention"),
            "node_ping_probe_samples",
            UpdateDataRetentionPolicy { ttl_hours: 1 },
        )
        .await
        .unwrap();
    let expired = db
        .store
        .record_ping_probe("n1", report(now - 86_400))
        .await
        .unwrap();
    assert_eq!(expired.accepted_samples, 0);
    assert_eq!(expired.skipped_samples, 1);
    let empty: i64 = sqlx::query_scalar("SELECT count(*) FROM node_ping_probe_samples")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(empty, 0);
    let fresh = db.store.record_ping_probe("n1", report(now)).await.unwrap();
    assert_eq!(fresh.accepted_samples, 1);
    for timestamp in [0, -1, now + 610] {
        assert!(matches!(
            db.store.record_ping_probe("n1", report(timestamp)).await,
            Err(StoreError::InvalidData(_))
        ));
    }
}
