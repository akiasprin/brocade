use super::*;
use brocade_deployment::traffic_limit::*;

fn policy() -> TrafficLimitPolicy {
    TrafficLimitPolicy {
        enabled: true,
        layer: TrafficLimitLayer::Both,
        rules: vec![
            TrafficLimitRule {
                threshold: TrafficLimitThreshold::QuotaPercent { basis_points: 9000 },
                rate_bps: 100_000_000,
            },
            TrafficLimitRule {
                threshold: TrafficLimitThreshold::Absolute {
                    bytes: "950".into(),
                },
                rate_bps: 512_000,
            },
        ],
    }
}
fn accounting(quota: Option<&str>, day: u8) -> UpdateNodeTrafficRequest {
    UpdateNodeTrafficRequest {
        cycle_kind: NodeTrafficCycleKind::Monthly,
        accounting_mode: Some(NodeTrafficAccountingMode::BidirectionalMax),
        reset_month: None,
        reset_day: day,
        quota_bytes: quota.map(str::to_owned),
        calibrated_total_bytes: None,
    }
}
fn reading(sequence: u64, at: i64, rx: u64, tx: u64) -> NodeTrafficReading {
    NodeTrafficReading {
        meter_id: "0123456789abcdef0123456789abcdef".into(),
        sequence,
        interface: "eth0".into(),
        boot_id: "11111111-2222-3333-4444-555555555555".into(),
        rx_bytes: rx,
        tx_bytes: tx,
        discontinuities: 0,
        sampled_at_unix_secs: Some(at),
        day_boundaries: vec![],
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn machine_traffic_limit_policy_is_scoped_audited_and_anchored() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store
        .update_node_traffic(&system_admin(), "n1", accounting(Some("1000"), 1))
        .await
        .unwrap();
    let readonly = AdminContext::new("vis", AdminRole::Readonly, Some("platform.acme".into()));
    assert!(matches!(
        db.store
            .update_node_traffic_limit(&readonly, "n1", policy())
            .await
            .unwrap_err(),
        StoreError::Forbidden(_)
    ));
    db.store
        .update_node_traffic_limit(&system_admin(), "n1", policy())
        .await
        .unwrap();
    db.store
        .update_node_traffic_limit(&system_admin(), "n1", policy())
        .await
        .unwrap();
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node_traffic_limit_events WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(events, 1, "repeating a policy is idempotent");
    assert!(db
        .store
        .update_node_traffic(&system_admin(), "n1", accounting(None, 1))
        .await
        .is_err());
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.store
        .record_node_traffic("n1", now, &reading(1, now - 2, 0, 0))
        .await
        .unwrap();
    db.store
        .record_node_traffic("n1", now, &reading(2, now - 1, 900, 500))
        .await
        .unwrap();
    let desired = db.store.node_traffic_limit_desired("n1").await.unwrap();
    assert_eq!(desired.revision, 1);
    assert_eq!(desired.policy, policy());
    let anchor = desired.anchor.unwrap();
    assert_eq!(
        (anchor.sequence, anchor.rx_bytes, anchor.tx_bytes),
        (2, 900, 500)
    );
    assert_eq!(
        desired.policy.rate(900, desired.quota_bytes),
        Some(100_000_000)
    );
    let observation = TrafficLimitObservation {
        revision: 1,
        phase: TrafficLimitPhase::Error,
        interface: Some("eth0".into()),
        rate_bps: Some(100_000_000),
        used_bytes: Some("900".into()),
        period_start_unix_secs: Some(anchor.period_start_unix_secs),
        error: Some("IFB unavailable".into()),
        system: Some(TrafficLayerObservation {
            phase: TrafficLimitPhase::Error,
            rate_bps: None,
            error: Some("IFB unavailable".into()),
        }),
        application: Some(TrafficLayerObservation {
            phase: TrafficLimitPhase::Limited,
            rate_bps: Some(100_000_000),
            error: None,
        }),
    };
    db.store
        .report_node_traffic_limit("n1", observation.clone())
        .await
        .unwrap();
    let mut stale = observation.clone();
    stale.revision = 0;
    stale.phase = TrafficLimitPhase::Disabled;
    db.store
        .report_node_traffic_limit("n1", stale)
        .await
        .unwrap();
    let view = db.store.node_traffic(&system_admin()).await.unwrap();
    assert_eq!(
        view.nodes
            .iter()
            .find(|n| n.node_id == "n1")
            .unwrap()
            .limit
            .observation,
        Some(observation)
    );
    let outside = AdminContext::new("outside", AdminRole::Readonly, Some("outside".into()));
    assert!(db
        .store
        .node_traffic(&outside)
        .await
        .unwrap()
        .nodes
        .iter()
        .all(|n| n.node_id != "n1"));
    db.store
        .update_node_traffic_limit(&system_admin(), "n1", TrafficLimitPolicy::default())
        .await
        .unwrap();
    db.store
        .update_node_traffic(&system_admin(), "n1", accounting(None, 1))
        .await
        .unwrap();
    db.store
        .update_node_traffic(&system_admin(), "n1", accounting(Some("1000"), 1))
        .await
        .unwrap();
    let actor = system_admin();
    let (remove, enable) = tokio::join!(
        db.store
            .update_node_traffic(&actor, "n1", accounting(None, 1)),
        db.store.update_node_traffic_limit(&actor, "n1", policy()),
    );
    assert_ne!(
        remove.is_ok(),
        enable.is_ok(),
        "quota removal and enabling percentages must serialize"
    );
    let desired = db.store.node_traffic_limit_desired("n1").await.unwrap();
    assert!(!desired.policy.enabled || desired.quota_bytes.is_some());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn machine_traffic_limit_offline_day_boundaries_replay_once_across_reset() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let today: i64 = sqlx::query_scalar("SELECT extract(epoch FROM date_trunc('day',now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC')::bigint")
        .fetch_one(db.pool()).await.unwrap();
    let day: i32 = sqlx::query_scalar("SELECT extract(day FROM now() AT TIME ZONE 'UTC')::integer")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.store
        .update_node_traffic(&system_admin(), "n1", accounting(Some("1000"), day as u8))
        .await
        .unwrap();
    db.store
        .record_node_traffic("n1", today + 180, &reading(1, today - 300, 0, 0))
        .await
        .unwrap();
    let mut next = reading(2, today + 30, 300, 600);
    next.day_boundaries.push(TrafficDayBoundary {
        day_start_unix_secs: today,
        rx_bytes: 100,
        tx_bytes: 200,
    });
    for _ in 0..2 {
        db.store
            .record_node_traffic("n1", today + 180, &next)
            .await
            .unwrap();
    }
    let rows: Vec<(String,String)> = sqlx::query_as("SELECT rx_bytes::text, tx_bytes::text FROM node_traffic_daily WHERE node_id='n1' ORDER BY day")
        .fetch_all(db.pool()).await.unwrap();
    assert_eq!(
        rows,
        [("100".into(), "200".into()), ("200".into(), "400".into())]
    );
    let gaps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM node_traffic_gaps WHERE node_id='n1' AND reason='report-gap'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(gaps, 0);
    let desired = db.store.node_traffic_limit_desired("n1").await.unwrap();
    let anchor = desired.anchor.unwrap();
    assert_eq!(anchor.period_start_unix_secs, today);
    assert_eq!((anchor.rx_bytes, anchor.tx_bytes), (200, 400));
    let mut malformed = next.clone();
    malformed.sequence = 3;
    malformed
        .day_boundaries
        .push(malformed.day_boundaries[0].clone());
    assert!(db
        .store
        .record_node_traffic("n1", today + 180, &malformed)
        .await
        .is_err());
}
