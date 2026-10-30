use super::*;
use brocade_deployment::source_traffic::{
    SourceTrafficCounter, SourceTrafficReport, TrafficTimeRange,
};

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn source_traffic_is_idempotent_scoped_and_keeps_historical_identity() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let bucket = now / 30 * 30;
    // Existing nodes have legacy bindings without chain_id; use only the frozen day rollup.
    let bindings = alice_usage_bindings();
    let generation = install_usage_generation(db.pool(), "n1", bindings, now - 300).await;
    sqlx::query("INSERT INTO usage_user_ingress_rollups (node_id,period_start,tenant_id,user_id,ingress_id,app_id,chain_id,uplink_bytes,downlink_bytes) VALUES ('n1',date_trunc('day',to_timestamp($1) AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong','platform.acme','alice','ing-a1b2','app-main','chn-a1b2-c3d4',100,900)")
        .bind(now as f64).execute(db.pool()).await.unwrap();
    let report = SourceTrafficReport {
        version: 1,
        epoch: "0123456789abcdef0123456789abcdef".into(),
        activation_unix_secs: now - 300,
        read_at_unix_secs: bucket + 1,
        usage_generation_id: generation,
        limited: false,
        counters: vec![
            SourceTrafficCounter {
                bucket,
                label: "alice@platform.acme#ing-a1b2".into(),
                source_ip: "192.0.2.1".into(),
                uplink_bytes: 10,
                downlink_bytes: 100,
            },
            SourceTrafficCounter {
                bucket,
                label: "alice@platform.acme#ing-a1b2".into(),
                source_ip: "2001:db8::1".into(),
                uplink_bytes: 20,
                downlink_bytes: 200,
            },
        ],
    };
    db.store
        .record_source_traffic_report("n1", report.clone())
        .await
        .unwrap();
    db.store
        .record_source_traffic_report("n1", report.clone())
        .await
        .unwrap();
    let details = db
        .store
        .node_inflow_details(
            &system_admin(),
            "n1",
            ("platform.acme", "alice"),
            SiteUsagePeriod::Day,
            None,
        )
        .await
        .unwrap();
    assert_eq!(details.rows.len(), 3);
    assert!(!details.accounting_mismatch);
    assert_eq!(
        details
            .rows
            .iter()
            .map(|r| r.traffic.downlink_bytes)
            .sum::<u64>(),
        900
    );
    assert_eq!(
        details
            .rows
            .iter()
            .find(|r| r.source_ip.is_none())
            .unwrap()
            .traffic
            .uplink_bytes,
        70
    );
    let mut next = report.clone();
    next.read_at_unix_secs += 1;
    next.counters[0].uplink_bytes += 5;
    db.store
        .record_source_traffic_report("n1", next.clone())
        .await
        .unwrap();
    let total: i64 =
        sqlx::query_scalar("SELECT sum(uplink_bytes)::bigint FROM source_traffic_daily")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(total, 35);
    next.read_at_unix_secs += 1;
    next.counters[0].uplink_bytes = 1;
    db.store
        .record_source_traffic_report("n1", next)
        .await
        .unwrap();
    let total: i64 =
        sqlx::query_scalar("SELECT sum(uplink_bytes)::bigint FROM source_traffic_daily")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(total, 35);
    sqlx::query("INSERT INTO chains (id,app_id,tenant_id,name,position) VALUES ('chain-new','app-main','platform.acme','New',1)").execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE ingresses SET chain_id='chain-new' WHERE id='ing-a1b2'")
        .execute(db.pool())
        .await
        .unwrap();
    let details = db
        .store
        .node_inflow_details(
            &system_admin(),
            "n1",
            ("platform.acme", "alice"),
            SiteUsagePeriod::Month,
            None,
        )
        .await
        .unwrap();
    assert!(details
        .rows
        .iter()
        .filter(|r| r.source_ip.is_some())
        .all(|r| r.traffic.chain_id.as_deref() == Some("chn-a1b2-c3d4")));
    // New frozen source attribution can be finer than the day's original billing chain.
    // Known bytes on the new chain must not also appear in the old chain's residual.
    sqlx::query("INSERT INTO source_traffic_daily (node_id,bucket,tenant_id,user_id,app_id,chain_id,ingress_id,source_ip,uplink_bytes,downlink_bytes) VALUES ('n1',(($1::bigint+28800)/86400*86400-28800),'platform.acme','alice','app-main','chain-new','ing-a1b2','192.0.2.99',10,100)")
        .bind(now).execute(db.pool()).await.unwrap();
    let rule:i64=sqlx::query_scalar("INSERT INTO forwarding_rules (tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port) VALUES ('platform.acme','alice','n1','Forwarding','tcp',24001,'192.0.2.2',443) RETURNING id").fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_usage_rollups (period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap) VALUES (to_timestamp((($1::bigint+28800)/86400*86400-28800)::double precision),'n1','platform.acme','alice',$2,3,7,false)")
        .bind(now).bind(rule).execute(db.pool()).await.unwrap();
    let details = db
        .store
        .node_inflow_details(
            &system_admin(),
            "n1",
            ("platform.acme", "alice"),
            SiteUsagePeriod::Day,
            None,
        )
        .await
        .unwrap();
    assert!(details.accounting_mismatch);
    assert_eq!(details.accounting_uplink_bytes, 103);
    assert_eq!(details.accounting_downlink_bytes, 907);
    assert_eq!(
        details
            .rows
            .iter()
            .map(|r| r.traffic.uplink_bytes)
            .sum::<u64>(),
        103
    );
    assert_eq!(
        details
            .rows
            .iter()
            .map(|r| r.traffic.downlink_bytes)
            .sum::<u64>(),
        907
    );
    let remainder = details
        .rows
        .iter()
        .find(|r| r.traffic.source == NodeInflowSource::Unattributed)
        .unwrap();
    assert!(remainder.source_ip.is_none());
    assert!(remainder.traffic.chain_id.is_none());
    assert_eq!(remainder.traffic.uplink_bytes, 55);
    assert_eq!(remainder.traffic.downlink_bytes, 500);
    let forwarding = details
        .rows
        .iter()
        .find(|r| r.traffic.source == NodeInflowSource::Forwarding)
        .unwrap();
    assert!(forwarding.source_ip.is_none());
    assert_eq!(forwarding.traffic.uplink_bytes, 3);
    assert_eq!(forwarding.traffic.downlink_bytes, 7);
    assert_eq!(
        details
            .rows
            .iter()
            .find(|r| r.source_ip.as_deref() == Some("192.0.2.99"))
            .unwrap()
            .traffic
            .chain_id
            .as_deref(),
        Some("chain-new")
    );
    for actor in [
        tenant_admin("platform.other"),
        ordinary_user("platform.acme"),
        AdminContext::new("public", AdminRole::Readonly, None),
    ] {
        assert!(matches!(
            db.store
                .node_inflow_details(
                    &actor,
                    "n1",
                    ("platform.acme", "alice"),
                    SiteUsagePeriod::Day,
                    None
                )
                .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    let mut duplicate = report;
    duplicate.read_at_unix_secs = bucket + 4;
    duplicate.counters.push(duplicate.counters[0].clone());
    assert!(db
        .store
        .record_source_traffic_report("n1", duplicate)
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn source_traffic_custom_range_uses_retained_windows_and_destinations_use_whole_hours() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let end = now / 30 * 30;
    let start = end - 60;
    let mut bindings = alice_usage_bindings();
    bindings["alice@platform.acme#ing-a1b2"]["chain_id"] = json!("chn-a1b2-c3d4");
    let generation = install_usage_generation(db.pool(), "n1", bindings, now - 300).await;
    for (bucket, up, down) in [
        (start, 10_i64, 100_i64),
        (start + 30, 20, 200),
        (start - 30, 9000, 9000),
        (end, 8000, 8000),
    ] {
        sqlx::query("INSERT INTO usage_samples (window_start,window_end,node_id,tenant_id,user_id,ingress_id,grant_label,app_id,generation_id,uplink_bytes,downlink_bytes) VALUES (to_timestamp($1),to_timestamp($1+30),'n1','platform.acme','alice','ing-a1b2','alice@platform.acme#ing-a1b2','app-main',$2,$3,$4)")
            .bind(bucket as f64).bind(generation).bind(up).bind(down).execute(db.pool()).await.unwrap();
    }
    let range = TrafficTimeRange {
        start_unix_secs: start,
        end_unix_secs: end,
    };
    let view = db
        .store
        .node_inflow_usage_range(&system_admin(), "n1", SiteUsagePeriod::Day, Some(range))
        .await
        .unwrap();
    assert_eq!(view.rows.len(), 1);
    assert_eq!(view.rows[0].uplink_bytes, 30);
    assert_eq!(view.rows[0].downlink_bytes, 300);
    assert_eq!(view.rows[0].chain_id.as_deref(), Some("chn-a1b2-c3d4"));
    let no_source = db
        .store
        .node_inflow_details(
            &system_admin(),
            "n1",
            ("platform.acme", "alice"),
            SiteUsagePeriod::Day,
            Some(range),
        )
        .await
        .unwrap();
    assert!(no_source.limited);
    assert_eq!(no_source.rows[0].source_ip, None);
    assert_eq!(no_source.rows[0].traffic.downlink_bytes, 300);
    for (bucket, down) in [
        (start / 3600 * 3600, 100_i64),
        (end / 3600 * 3600 + 3600, 8000),
    ] {
        sqlx::query("INSERT INTO user_site_usage_hourly (bucket,node_id,tenant_id,user_id,site_id,uplink_bytes,downlink_bytes) VALUES ($1,'n1','platform.acme','alice','youtube',0,$2)")
            .bind(bucket).bind(down).execute(db.pool()).await.unwrap();
    }
    let rule:i64=sqlx::query_scalar("INSERT INTO forwarding_rules (tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port) VALUES ('platform.acme','alice','n1','Forwarding','tcp',24001,'192.0.2.2',443) RETURNING id").fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_usage_samples (node_id,rule_id,tenant_id,user_id,read_at,window_start,uplink_bytes,downlink_bytes) VALUES ('n1',$1,'platform.acme','alice',to_timestamp($2::bigint+30),to_timestamp($2::bigint),3,7)")
        .bind(rule).bind(start).execute(db.pool()).await.unwrap();
    let sites = db
        .store
        .node_site_usage_range(&system_admin(), "n1", SiteUsagePeriod::Day, Some(range))
        .await
        .unwrap();
    assert_eq!(
        sites
            .usage
            .rows
            .iter()
            .find(|row| row.site_id == "youtube")
            .unwrap()
            .downlink_bytes,
        100
    );
    assert_eq!(
        sites
            .usage
            .rows
            .iter()
            .find(|row| row.site_id == "forwarding")
            .unwrap()
            .downlink_bytes,
        7
    );
    assert_eq!(sites.usage.range_start_unix_secs, start / 3600 * 3600);
    assert!(sites.usage.range_end_unix_secs <= (end + 3599) / 3600 * 3600);
    assert!(db
        .store
        .node_inflow_usage_range(
            &system_admin(),
            "n1",
            SiteUsagePeriod::Day,
            Some(TrafficTimeRange {
                start_unix_secs: end,
                end_unix_secs: start
            })
        )
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn source_traffic_chain_moves_reconcile_across_historical_app_rows_once() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO usage_user_ingress_rollups(node_id,period_start,tenant_id,user_id,ingress_id,app_id,chain_id,uplink_bytes,downlink_bytes)
        SELECT 'n1',date_trunc('day',now() AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong','platform.acme','alice',ingress,app,'chn-a1b2-c3d4',0,bytes
        FROM (VALUES ('before','app-old',100),('after','app-main',200)) entries(ingress,app,bytes)")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO usage_user_node_rollups(node_id,period_start,tenant_id,user_id,uplink_bytes,downlink_bytes)
        VALUES('n1',date_trunc('day',now() AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong','platform.acme','alice',0,300)")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO source_traffic_daily(node_id,bucket,tenant_id,user_id,app_id,chain_id,ingress_id,source_ip,uplink_bytes,downlink_bytes)
        SELECT 'n1',extract(epoch FROM date_trunc('day',now() AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong')::bigint,
        'platform.acme','alice',app,'chn-a1b2-c3d4',ingress,'192.0.2.1',0,bytes
        FROM (VALUES ('before','app-old',120),('after','app-main',80)) entries(ingress,app,bytes)")
        .execute(db.pool()).await.unwrap();
    let details = db
        .store
        .node_inflow_details(
            &system_admin(),
            "n1",
            ("platform.acme", "alice"),
            SiteUsagePeriod::Day,
            None,
        )
        .await
        .unwrap();
    assert!(
        !details.accounting_mismatch,
        "a group move alone must not imply a different chain ledger"
    );
    assert_eq!(details.accounting_downlink_bytes, 300);
    assert_eq!(
        details
            .rows
            .iter()
            .map(|r| r.traffic.downlink_bytes)
            .sum::<u64>(),
        300
    );
    assert_eq!(
        details
            .rows
            .iter()
            .filter(|r| r.source_ip.is_none())
            .map(|r| r.traffic.downlink_bytes)
            .sum::<u64>(),
        100
    );
    // Real over-reporting remains visible even when the chain spans several groups.
    sqlx::query("UPDATE source_traffic_daily SET downlink_bytes=300 WHERE app_id='app-main'")
        .execute(db.pool())
        .await
        .unwrap();
    let details = db
        .store
        .node_inflow_details(
            &system_admin(),
            "n1",
            ("platform.acme", "alice"),
            SiteUsagePeriod::Day,
            None,
        )
        .await
        .unwrap();
    assert!(details.accounting_mismatch);
    assert_eq!(
        details
            .rows
            .iter()
            .map(|r| r.traffic.downlink_bytes)
            .sum::<u64>(),
        420
    );
}
