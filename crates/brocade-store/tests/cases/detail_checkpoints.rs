use super::*;
use brocade_deployment::other_domain::{OtherDomainCounter, OtherDomainPolicy, OtherDomainReport};
use brocade_deployment::sniffing_failed::{SniffingFailedCounter, SniffingFailedPolicy};

async fn record_checkpoint(db: &TestPg, failed: bool, report: OtherDomainReport) {
    if failed {
        db.store
            .record_sniffing_failed_report(
                "n1",
                report.map_counters(|r| SniffingFailedCounter {
                    counter_id: r.counter_id,
                    bucket: r.bucket,
                    label: r.label,
                    target: r.domain,
                    uplink_bytes: r.uplink_bytes,
                    downlink_bytes: r.downlink_bytes,
                }),
            )
            .await
            .unwrap();
    } else {
        db.store
            .record_other_domain_report("n1", report)
            .await
            .unwrap();
    }
}

async fn query_history(
    db: &TestPg,
    failed: bool,
    node: bool,
    period: SiteUsagePeriod,
) -> brocade_deployment::other_domain::UserDetailUsage<u64> {
    match (failed, node) {
        (false, false) => db
            .store
            .user_other_domains(&system_admin(), "platform.acme", "alice", period)
            .await
            .unwrap()
            .map_rows(|r| r.uplink_bytes + r.downlink_bytes),
        (false, true) => db
            .store
            .node_other_domains(&system_admin(), "n1", period)
            .await
            .unwrap()
            .map_rows(|r| r.uplink_bytes + r.downlink_bytes),
        (true, false) => db
            .store
            .user_sniffing_failed_targets(&system_admin(), "platform.acme", "alice", period)
            .await
            .unwrap()
            .map_rows(|r| r.uplink_bytes + r.downlink_bytes),
        (true, true) => db
            .store
            .node_sniffing_failed_targets(&system_admin(), "n1", period)
            .await
            .unwrap()
            .map_rows(|r| r.uplink_bytes + r.downlink_bytes),
    }
}

async fn configured_history_retention_case(failed: bool) {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO admin_operators (id,display_name,role,password_hash) VALUES ('detail-admin','Detail admin','system-admin','test-only')")
        .execute(db.pool()).await.unwrap();
    let admin = AdminContext::system_admin("detail-admin");
    let (table, key, other_table) = if failed {
        (
            "user_sniffing_failed_hourly",
            "target",
            "user_other_domain_hourly",
        )
    } else {
        (
            "user_other_domain_hourly",
            "domain",
            "user_sniffing_failed_hourly",
        )
    };
    let name = if failed {
        "tcp://192.0.2.1:443"
    } else {
        "history.example"
    };
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let hour = now / 3600 * 3600;
    let generation =
        install_usage_generation(db.pool(), "n1", alice_usage_bindings(), now - 10 * 86_400).await;
    let mut report = OtherDomainReport {
        version: 2,
        epoch: "a".repeat(32),
        activation_unix_secs: now - 10 * 86_400,
        read_at_unix_secs: now,
        usage_generation_id: generation,
        limited: false,
        counters: vec![OtherDomainCounter {
            counter_id: 1,
            bucket: hour,
            label: "alice@platform.acme#ing-a1b2".into(),
            domain: if failed {
                "udp://192.0.2.2:53"
            } else {
                "current.example"
            }
            .into(),
            uplink_bytes: 10,
            downlink_bytes: 0,
        }],
    };
    record_checkpoint(&db, failed, report.clone()).await;
    // Seed real hourly history independently of the short checkpoint replay window.
    let insert = format!("INSERT INTO {table} (bucket,node_id,tenant_id,user_id,{key},uplink_bytes,downlink_bytes) SELECT $1 - age * 3600,'n1','platform.acme','alice',$2,1,2 FROM generate_series(0,191) age");
    sqlx::query(&insert)
        .bind(hour)
        .bind(name)
        .execute(db.pool())
        .await
        .unwrap();
    let seed_buckets: Vec<_> = (0..192).map(|age| hour - age * 3600).collect();
    let policy = db
        .store
        .data_retention()
        .await
        .unwrap()
        .policies
        .into_iter()
        .find(|p| p.table_name == table)
        .unwrap();
    assert_eq!(policy.ttl_hours, 24);
    assert_eq!(policy.default_ttl_hours, 24);

    for hours in [24_u32, 7 * 24, 3 * 24, 6] {
        if hours != 24 {
            db.store
                .update_data_retention(
                    &admin,
                    table,
                    UpdateDataRetentionPolicy { ttl_hours: hours },
                )
                .await
                .unwrap();
            // A new upload must preserve all history inside the configured lifetime.
            report.counters[0].uplink_bytes += 5;
            record_checkpoint(&db, failed, report.clone()).await;
            let retained: i64 =
                sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE {key}=$1"))
                    .bind(name)
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(retained, i64::from(hours));
            assert_eq!(
                db.store
                    .retention_schedule()
                    .await
                    .unwrap()
                    .ttl_hours(other_table),
                24
            );
        }
        for period in [
            SiteUsagePeriod::Day,
            SiteUsagePeriod::Week,
            SiteUsagePeriod::Month,
        ] {
            let sites = db
                .store
                .user_site_usage(&system_admin(), "platform.acme", "alice", period)
                .await
                .unwrap();
            for node in [false, true] {
                let detail = query_history(&db, failed, node, period).await;
                let cutoff =
                    (detail.range_end_unix_secs - i64::from(hours) * 3600 + 3599) / 3600 * 3600;
                assert_eq!(
                    detail.range_start_unix_secs,
                    sites.range_start_unix_secs.max(cutoff)
                );
                assert_eq!(
                    detail.first_record_unix_secs,
                    seed_buckets
                        .iter()
                        .copied()
                        .filter(|&bucket| bucket >= detail.range_start_unix_secs)
                        .min(),
                    "first record must follow the actual retained rows in {table}"
                );
                let expected = seed_buckets
                    .iter()
                    .filter(|&&bucket| bucket >= detail.range_start_unix_secs)
                    .count() as u64
                    * 3
                    + report.counters[0].uplink_bytes;
                assert_eq!(
                    detail.rows.iter().sum::<u64>(),
                    expected,
                    "{table}, {hours} hours, {period:?}, node={node}"
                );
            }
        }
        if hours == 7 * 24 {
            let mut expired = report.clone();
            expired.read_at_unix_secs -= 48 * 3600;
            expired.counters[0].bucket -= 48 * 3600;
            expired.counters[0].counter_id = 99;
            expired.counters[0].uplink_bytes = 1_000_000;
            record_checkpoint(&db, failed, expired).await;
            let total: i64 =
                sqlx::query_scalar(&format!("SELECT sum(uplink_bytes)::bigint FROM {table}"))
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(
                total,
                i64::from(hours) + report.counters[0].uplink_bytes as i64,
                "longer history must not extend checkpoint replay acceptance"
            );
            sqlx::query(&format!("INSERT INTO {table} (bucket,node_id,tenant_id,user_id,{key},uplink_bytes,downlink_bytes) VALUES ($1,'n1','platform.acme','alice',$2,1,2)"))
                .bind(hour - 8 * 24 * 3600).bind(name).execute(db.pool()).await.unwrap();
            let cleanup = db.store.run_data_retention().await.unwrap();
            let result = cleanup.iter().find(|r| r.table_name == table).unwrap();
            assert_eq!(result.error_code, None);
            assert_eq!(result.deleted_rows, 1);
            // The older usage cleanup entry point must honor the same live history policy.
            db.store.prune_usage_samples(7).await.unwrap();
            let retained: i64 =
                sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE {key}=$1"))
                    .bind(name)
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(
                retained,
                7 * 24,
                "scheduled cleanup must keep seven days too"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn detail_history_domain_retention_is_configurable_for_uploads_queries_and_cleanup() {
    configured_history_retention_case(false).await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn detail_history_failed_retention_is_configurable_for_uploads_queries_and_cleanup() {
    configured_history_retention_case(true).await;
}

async fn first_record_case(failed: bool) {
    use brocade_deployment::source_traffic::TrafficTimeRange;

    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let budget = OtherDomainPolicy {
        enabled: true,
        top_n: 1,
        ..Default::default()
    };
    if failed {
        db.store
            .update_sniffing_failed_policy(
                &system_admin(),
                SniffingFailedPolicy::from_budget(budget),
            )
            .await
            .unwrap();
    } else {
        db.store
            .update_other_domain_policy(&system_admin(), budget)
            .await
            .unwrap();
    }
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let hour = now / 3600 * 3600;
    let generation = install_usage_generation(db.pool(), "n1", alice_usage_bindings(), now).await;
    record_checkpoint(
        &db,
        failed,
        OtherDomainReport {
            version: 2,
            epoch: "a".repeat(32),
            activation_unix_secs: now,
            read_at_unix_secs: now,
            usage_generation_id: generation,
            limited: false,
            counters: Vec::new(),
        },
    )
    .await;
    for node in [false, true] {
        let empty = query_history(&db, failed, node, SiteUsagePeriod::Month).await;
        assert_eq!(empty.observed_since_unix_secs, Some(now));
        assert_eq!(empty.first_record_unix_secs, None);
    }
    sqlx::raw_sql(
        "INSERT INTO tenants (id,name) VALUES ('platform.other','Other');
         INSERT INTO users (tenant_id,id,uuid) VALUES
         ('platform.acme','bob','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'),
         ('platform.other','alice','bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb');",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let (table, key) = if failed {
        ("user_sniffing_failed_hourly", "target")
    } else {
        ("user_other_domain_hourly", "domain")
    };
    // The oldest visible record is deliberately outside Top 1. Earlier rows from another
    // tenant/user, outside retention, and after a custom range must not set its timestamp.
    for (index, (age, tenant, user, bytes)) in [
        (48, "platform.acme", "alice", 1_i64),
        (5, "platform.other", "alice", 1),
        (4, "platform.acme", "bob", 1),
        (3, "platform.acme", "alice", 1),
        (1, "platform.acme", "alice", 100),
        (0, "platform.acme", "alice", 1000),
    ]
    .into_iter()
    .enumerate()
    {
        let name = if failed {
            format!("tcp://192.0.2.{}:443", index + 1)
        } else {
            format!("host{index}.example")
        };
        sqlx::query(&format!("INSERT INTO {table} (bucket,node_id,tenant_id,user_id,{key},uplink_bytes,downlink_bytes) VALUES ($1,'n1',$2,$3,$4,$5,0)"))
            .bind(hour - age * 3600).bind(tenant).bind(user).bind(name).bind(bytes)
            .execute(db.pool()).await.unwrap();
    }
    let user = query_history(&db, failed, false, SiteUsagePeriod::Month).await;
    let first = [hour - 3 * 3600, hour - 3600, hour]
        .into_iter()
        .find(|&bucket| bucket >= user.range_start_unix_secs);
    assert_eq!(user.first_record_unix_secs, first);
    assert_eq!(user.rows, vec![1000]);

    for (start, end, expected) in [
        (hour - 24 * 3600, hour, Some(hour - 4 * 3600)),
        (hour - 2 * 3600, hour, Some(hour - 3600)),
        (hour - 12 * 3600, hour - 10 * 3600, None),
    ] {
        let range = Some(TrafficTimeRange {
            start_unix_secs: start,
            end_unix_secs: end,
        });
        let actor = tenant_admin("platform.acme");
        let node = if failed {
            db.store
                .node_sniffing_failed_targets_range(&actor, "n1", SiteUsagePeriod::Day, range)
                .await
                .unwrap()
                .map_rows(|r| r.uplink_bytes + r.downlink_bytes)
        } else {
            db.store
                .node_other_domains_range(&actor, "n1", SiteUsagePeriod::Day, range)
                .await
                .unwrap()
                .map_rows(|r| r.uplink_bytes + r.downlink_bytes)
        };
        assert_eq!(node.first_record_unix_secs, expected);
        assert_eq!(
            node.rows,
            if expected.is_some() {
                vec![100]
            } else {
                vec![]
            }
        );
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn detail_history_domain_first_record_uses_scoped_rows_before_top_n() {
    first_record_case(false).await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn detail_history_failed_first_record_uses_scoped_rows_before_top_n() {
    first_record_case(true).await;
}

async fn checkpoint_replay_case(failed: bool) {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let budget = OtherDomainPolicy {
        enabled: true,
        top_n: 1,
        max_domains_per_user: 1,
        node_capacity: 1,
        report_interval_secs: 30,
    };
    if failed {
        db.store
            .update_sniffing_failed_policy(
                &system_admin(),
                SniffingFailedPolicy::from_budget(budget),
            )
            .await
            .unwrap();
    } else {
        db.store
            .update_other_domain_policy(&system_admin(), budget)
            .await
            .unwrap();
    }
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let base = now - 3 * 3600;
    let generation =
        install_usage_generation(db.pool(), "n1", alice_usage_bindings(), base - 60).await;
    let name = |second: bool| -> &str {
        match (failed, second) {
            (false, false) => "a.example",
            (false, true) => "b.example",
            (true, false) => "tcp://192.0.2.1:443",
            (true, true) => "udp://192.0.2.2:53",
        }
    };
    let make = |id, second, up| OtherDomainReport {
        version: 2,
        epoch: "a".repeat(32),
        activation_unix_secs: base - 30,
        read_at_unix_secs: base,
        usage_generation_id: generation,
        limited: false,
        counters: vec![OtherDomainCounter {
            counter_id: id,
            bucket: base / 3600 * 3600,
            label: "alice@platform.acme#ing-a1b2".into(),
            domain: name(second).into(),
            uplink_bytes: up,
            downlink_bytes: 0,
        }],
    };
    let first = make(1, false, 10);
    let second = make(2, true, 20);
    let reentered = make(3, false, 7);
    for report in [&first, &second, &reentered] {
        record_checkpoint(&db, failed, report.clone()).await;
    }
    let (history, heads, state) = if failed {
        (
            "user_sniffing_failed_hourly",
            "sniffing_failed_counter_heads",
            "node_sniffing_failed_state",
        )
    } else {
        (
            "user_other_domain_hourly",
            "other_domain_counter_heads",
            "node_other_domain_state",
        )
    };
    // Literal table names above are the entire allow-list; no external SQL identifiers.
    let total_sql = format!("SELECT sum(uplink_bytes)::bigint FROM {history} WHERE node_id='n1'");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(&total_sql)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        37
    );
    sqlx::query("INSERT INTO admin_operators (id,display_name,role,password_hash) VALUES ('legacy-test','Legacy test','system-admin','test-hash')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO data_retention_policies (table_name,ttl_hours,updated_by) VALUES ($1,2,'legacy-test') ON CONFLICT (table_name) DO UPDATE SET ttl_hours=2")
        .bind(heads).execute(db.pool()).await.unwrap();
    let schedule = db.store.retention_schedule().await.unwrap();
    assert_eq!(
        schedule.ttl_hours(heads),
        24,
        "old retention overrides must respect replay safety"
    );
    db.store.run_data_retention().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {heads}"))
            .fetch_one(db.pool())
            .await
            .unwrap(),
        3,
        "deduplication must survive the legacy two-hour housekeeping cutoff"
    );
    // Response loss, Agent restart and replay in a different order must add no bytes.
    for report in [&reentered, &first, &second] {
        record_checkpoint(&db, failed, report.clone()).await;
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(&total_sql)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        37
    );
    let mut newer = first.clone();
    newer.counters[0].uplink_bytes = 15;
    newer.counters[0].downlink_bytes = 5;
    record_checkpoint(&db, failed, newer.clone()).await;
    newer.counters[0].uplink_bytes = 12;
    newer.counters[0].downlink_bytes = 8;
    record_checkpoint(&db, failed, newer).await;
    record_checkpoint(&db, failed, first.clone()).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(&total_sql)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        42
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(&format!(
            "SELECT sum(downlink_bytes)::bigint FROM {history}"
        ))
        .fetch_one(db.pool())
        .await
        .unwrap(),
        8
    );
    assert!(
        !sqlx::query_scalar::<_, bool>(&format!("SELECT limited FROM {state} WHERE node_id='n1'"))
            .fetch_one(db.pool())
            .await
            .unwrap(),
        "ordinary cumulative replay must not masquerade as observation loss"
    );
    let mut new_epoch = make(1, false, 4);
    new_epoch.epoch = "b".repeat(32);
    new_epoch.activation_unix_secs = now - 10;
    new_epoch.read_at_unix_secs = now;
    new_epoch.counters[0].bucket = now / 3600 * 3600;
    record_checkpoint(&db, failed, new_epoch).await;
    record_checkpoint(&db, failed, first).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(&total_sql)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        46
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(&format!("SELECT epoch FROM {state} WHERE node_id='n1'"))
            .fetch_one(db.pool())
            .await
            .unwrap(),
        "b".repeat(32),
        "delayed old epochs cannot move coverage state backwards"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn detail_checkpoint_domain_replay_is_idempotent_after_eviction_and_retention() {
    checkpoint_replay_case(false).await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn detail_checkpoint_failed_replay_is_idempotent_after_eviction_and_retention() {
    checkpoint_replay_case(true).await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn detail_checkpoint_upgrade_transfers_legacy_baseline_once() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let generation =
        install_usage_generation(db.pool(), "n1", alice_usage_bindings(), now - 120).await;
    let mut report = OtherDomainReport {
        version: 1,
        epoch: "a".repeat(32),
        activation_unix_secs: now - 60,
        read_at_unix_secs: now - 30,
        usage_generation_id: generation,
        limited: false,
        counters: vec![OtherDomainCounter {
            counter_id: 0,
            bucket: (now - 30) / 3600 * 3600,
            label: "alice@platform.acme#ing-a1b2".into(),
            domain: "a.example".into(),
            uplink_bytes: 10,
            downlink_bytes: 0,
        }],
    };
    let mut second_legacy = report.counters[0].clone();
    second_legacy.domain = "b.example".into();
    second_legacy.uplink_bytes = 20;
    report.counters.push(second_legacy);
    record_checkpoint(&db, false, report.clone()).await;
    report.version = 2;
    report.counters[0].counter_id = 100;
    report.counters[1].counter_id = 200;
    let mut second_segment = report.counters[1].clone();
    second_segment.counter_id = 201;
    second_segment.uplink_bytes = 4;
    // Coalesced segments may arrive in any row order. Only the earliest admission
    // inherits the legacy baseline; the later small target must retain every byte.
    report.counters.insert(1, second_segment);
    // Equal values still have to persist the transferred v2 baseline.
    record_checkpoint(&db, false, report.clone()).await;
    report.counters.truncate(1);
    let mut reentered = report.clone();
    reentered.counters[0].counter_id = 101;
    reentered.counters[0].uplink_bytes = 3;
    record_checkpoint(&db, false, reentered).await;
    report.counters[0].uplink_bytes = 15;
    record_checkpoint(&db, false, report.clone()).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT sum(uplink_bytes)::bigint FROM user_other_domain_hourly"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        42,
        "an Agent upgrade must count only new bytes, and a later admission must start at zero"
    );
    report.version = 1;
    report.counters[0].counter_id = 0;
    assert!(
        db.store
            .record_other_domain_report("n1", report)
            .await
            .is_err(),
        "a v1 downgrade within a v2 epoch cannot safely identify readmissions"
    );
}
