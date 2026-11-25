use super::*;
use brocade_store::UpdateNodeNotificationSettings;
use sha2::{Digest, Sha384};

const OLD: &str = "f3d2016a0ce40fcb920794d6e587dd291347c5c89c4435915c17be001a438875fb29b0608fe86a396ce534e9aff1d0b3";

async fn install_previous(db: &TestPg, clash_only: bool) {
    let directory = std::env::temp_dir().join(format!(
        "brocade-notification-upgrade-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    for item in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")).unwrap() {
        let item = item.unwrap();
        if item.file_name().to_string_lossy().as_ref() >= "0008_" {
            continue;
        }
        let mut data = std::fs::read_to_string(item.path()).unwrap();
        if item.file_name().to_string_lossy().starts_with("0007_") {
            if clash_only {
                data = data
                    .split_once("\n-- Optional host-egress maintenance;")
                    .unwrap()
                    .0
                    .to_owned();
            }
            let checksum = if clash_only {
                "d166d16cce814340fb2c9c30df6aed5d6bb2577b148043b76d2548b9b9fbaf180607a7f5ad1751e6c12ca6e9c3af3f6a"
            } else {
                OLD
            };
            assert_eq!(format!("{:x}", Sha384::digest(data.as_bytes())), checksum);
        }
        std::fs::write(directory.join(item.file_name()), data).unwrap();
    }
    sqlx::migrate::Migrator::new(directory.as_path())
        .await
        .unwrap()
        .run(db.pool())
        .await
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    insert_minimal_fixture(db.pool()).await;
}

async fn receipts(db: &TestPg) -> String {
    sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(m) ORDER BY version)::text FROM _sqlx_migrations m",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
}

async fn install_deployed_notification_0007(db: &TestPg) {
    let directory = std::env::temp_dir().join(format!(
        "brocade-deployed-notification-0007-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    for item in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")).unwrap() {
        let item = item.unwrap();
        if item.file_name().to_string_lossy().as_ref() < "0007_" {
            std::fs::copy(item.path(), directory.join(item.file_name())).unwrap();
        }
    }
    std::fs::write(
        directory.join("0007_user_clash_rules.sql"),
        include_str!("../fixtures/migrations/deployed_0007_with_notifications.sql"),
    )
    .unwrap();
    sqlx::migrate::Migrator::new(directory.as_path())
        .await
        .unwrap()
        .run(db.pool())
        .await
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO node_notification_preferences(node_id,public_ip_changes,changed_by) VALUES('n1',FALSE,'deployed-operator')")
        .execute(db.pool()).await.unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_notifications_exact_deployed_0007_converts_without_losing_preferences() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install_deployed_notification_0007(&db).await;
    let before_receipts: Vec<serde_json::Value> =
        serde_json::from_str(&receipts(&db).await).unwrap();
    let preference: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(p) FROM node_notification_preferences p WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    let check = db.store.check_prerelease_0007_upgrade().await.unwrap();
    let notification_schema: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_build_object(
           'columns',(SELECT jsonb_agg(jsonb_build_array(a.attname,format_type(a.atttypid,a.atttypmod),a.attnotnull,pg_get_expr(d.adbin,d.adrelid)) ORDER BY a.attnum) FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE a.attrelid='public.node_notification_preferences'::regclass AND a.attnum>0 AND NOT a.attisdropped),
           'constraints',(SELECT jsonb_agg(jsonb_build_array(k.conname,pg_get_constraintdef(k.oid)) ORDER BY k.conname) FROM pg_constraint k WHERE k.conrelid='public.node_notification_preferences'::regclass AND k.convalidated),
           'indexes',(SELECT jsonb_agg(jsonb_build_array(pg_get_indexdef(i.indexrelid),i.indisvalid,i.indisready) ORDER BY pg_get_indexdef(i.indexrelid)) FROM pg_index i WHERE i.indrelid='public.node_notification_preferences'::regclass))"
    ).fetch_one(db.pool()).await.unwrap();
    assert!(check.compatible, "{check:?}; schema={notification_schema}");
    assert_eq!(check.pending_versions, [7, 8, 9]);
    assert_eq!(
        serde_json::from_str::<Vec<serde_json::Value>>(&receipts(&db).await).unwrap(),
        before_receipts,
        "the deployed-history preflight must be read-only"
    );

    db.store.migrate_prerelease_0007_schema().await.unwrap();
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    let after_receipts: Vec<serde_json::Value> =
        serde_json::from_str(&receipts(&db).await).unwrap();
    assert_eq!(after_receipts.len(), 9);
    assert_eq!(&after_receipts[..6], &before_receipts[..6]);
    let mut expected_seventh = before_receipts[6].clone();
    expected_seventh["checksum"] = serde_json::Value::String(format!("\\x{OLD}"));
    assert_eq!(after_receipts[6], expected_seventh);
    assert_eq!(
        after_receipts[6]["description"], before_receipts[6]["description"],
        "conversion must preserve receipt metadata other than the corrected checksum"
    );
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT to_jsonb(p) FROM node_notification_preferences p WHERE node_id='n1'"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        preference
    );
    let after = receipts(&db).await;
    db.store.migrate_prerelease_0007_schema().await.unwrap();
    assert_eq!(after, receipts(&db).await);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_notifications_default_on_persist_and_filter_only_the_bell_before_its_limit() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,dns_kind)
        VALUES('n2','platform.acme','Node 2','n2.example.net','10.66.0.2','private-2','public-2',51820,'system')")
        .execute(db.pool()).await.unwrap();
    assert!(db
        .store
        .list_node_agent_states(&system_admin())
        .await
        .unwrap()
        .nodes
        .iter()
        .all(|node| node.public_ip_change_notifications));
    let before_model = db.store.materialize_snapshot(None).await.unwrap();
    db.store
        .configure_webhook_notification_channel(true)
        .await
        .unwrap();
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut observation = NodePublicIpObservation {
        observed_at_unix_secs: now,
        family: PublicIpFamily::V4,
        ip: "1.1.1.1".into(),
        country_code: Some("AU".into()),
    };
    db.store
        .record_node_public_ip("n1", &observation)
        .await
        .unwrap();
    observation.ip = "8.8.8.8".into();
    db.store
        .record_node_public_ip("n1", &observation)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE node_public_ip_state SET candidate_first_seen_at=now()-interval '11 seconds'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.store
        .record_node_public_ip("n1", &observation)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO machine_events(node_id,event_kind,family,previous_value,current_value,occurred_at)
        VALUES('n2','public_ip_changed',6,'2001:db8::1','2001:db8::2',now()),
        ('n1','node_online',NULL,'waiting','online',now());
        INSERT INTO machine_events(node_id,event_kind,family,previous_value,current_value,occurred_at)
        SELECT 'n1','public_ip_changed',4,'1.1.1.1','8.8.4.4',now()+make_interval(secs=>i) FROM generate_series(1,10) i;")
        .execute(db.pool()).await.unwrap();
    let history = db
        .store
        .node_public_ip_history(&system_admin(), "n1", 7)
        .await
        .unwrap();
    let deliveries: i64 = sqlx::query_scalar("SELECT count(*) FROM notification_deliveries")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.store
        .update_node_notification_settings(
            &system_admin(),
            "n1",
            UpdateNodeNotificationSettings {
                public_ip_changes: false,
            },
        )
        .await
        .unwrap();
    let view = db
        .store
        .list_node_agent_states(&system_admin())
        .await
        .unwrap();
    assert!(
        !view
            .nodes
            .iter()
            .find(|n| n.node_id == "n1")
            .unwrap()
            .public_ip_change_notifications
    );
    assert!(
        view.nodes
            .iter()
            .find(|n| n.node_id == "n2")
            .unwrap()
            .public_ip_change_notifications
    );
    let reader = AdminContext::new("reader", AdminRole::Readonly, Some("platform.acme".into()));
    assert!(
        !db.store
            .list_node_agent_states(&reader)
            .await
            .unwrap()
            .nodes[0]
            .public_ip_change_notifications
    );
    for actor in [system_admin(), reader] {
        let page = db.store.machine_events(&actor, 100).await.unwrap();
        assert!(!page
            .events
            .iter()
            .any(|event| event.node_id == "n1" && event.event_kind == "public_ip_changed"));
        assert!(page
            .events
            .iter()
            .any(|event| event.node_id == "n1" && event.event_kind == "node_online"));
        assert!(page
            .events
            .iter()
            .any(|event| event.node_id == "n2" && event.event_kind == "public_ip_changed"));
        assert_eq!(
            db.store
                .machine_events(&actor, 1)
                .await
                .unwrap()
                .events
                .len(),
            1
        );
    }
    assert_eq!(
        db.store
            .node_public_ip_history(&system_admin(), "n1", 7)
            .await
            .unwrap(),
        history
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notification_deliveries")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        deliveries
    );
    assert_eq!(
        db.store.materialize_snapshot(None).await.unwrap(),
        before_model
    );
    let reconnected = PgStore::connect(&db.url).await.unwrap();
    assert!(
        !reconnected
            .list_node_agent_states(&system_admin())
            .await
            .unwrap()
            .nodes[0]
            .public_ip_change_notifications
    );
    db.store
        .update_node_notification_settings(
            &system_admin(),
            "n1",
            UpdateNodeNotificationSettings {
                public_ip_changes: true,
            },
        )
        .await
        .unwrap();
    assert!(db
        .store
        .machine_events(&system_admin(), 100)
        .await
        .unwrap()
        .events
        .iter()
        .any(|event| event.node_id == "n1" && event.event_kind == "public_ip_changed"));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_notifications_require_system_admin_and_do_not_create_missing_nodes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    for role in [
        AdminRole::Readonly,
        AdminRole::Editor,
        AdminRole::Publisher,
        AdminRole::TenantAdmin,
    ] {
        let actor = AdminContext::new("scoped", role, Some("platform.acme".into()));
        assert!(matches!(
            db.store
                .update_node_notification_settings(
                    &actor,
                    "n1",
                    UpdateNodeNotificationSettings {
                        public_ip_changes: false
                    }
                )
                .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    assert!(matches!(
        db.store
            .update_node_notification_settings(
                &system_admin(),
                "missing",
                UpdateNodeNotificationSettings {
                    public_ip_changes: false
                }
            )
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM node_notification_preferences")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

async fn check_notification_compat(tag: &str) {
    let Some(db) = TestPg::start_if_enabled_with_tag(tag).await else {
        return;
    };
    install_previous(&db, false).await;
    let before = receipts(&db).await;
    let model = db.store.materialize_snapshot(None).await.unwrap();
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    let check = db.store.check_prerelease_0007_upgrade().await.unwrap();
    assert!(check.compatible, "{check:?}");
    assert_eq!(check.pending_versions, [8, 9]);
    assert_eq!(receipts(&db).await, before);
    sqlx::raw_sql("CREATE FUNCTION reject_notification_ddl() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN
            IF to_regclass('public.cvm_spec_artifacts') IS NOT NULL THEN RAISE EXCEPTION 'injected artifact DDL failure'; END IF;
            END $$; CREATE EVENT TRIGGER reject_notification_ddl ON ddl_command_end EXECUTE FUNCTION reject_notification_ddl();")
            .execute(db.pool()).await.unwrap();
    assert!(db.store.migrate_prerelease_0007_schema().await.is_err());
    assert_eq!(receipts(&db).await, before);
    assert!(sqlx::query_scalar::<_, bool>(
        "SELECT to_regclass('node_notification_preferences') IS NULL AND to_regclass('cvm_image_manifests') IS NULL"
    )
    .fetch_one(db.pool())
    .await
    .unwrap());
    sqlx::raw_sql(
        "DROP EVENT TRIGGER reject_notification_ddl; DROP FUNCTION reject_notification_ddl();",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.store.migrate_prerelease_0007_schema().await.unwrap();
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    let old_receipts: String = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(m) ORDER BY version)::text FROM _sqlx_migrations m WHERE version<=7")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        old_receipts, before,
        "published receipts must remain byte-for-byte unchanged"
    );
    assert_eq!(db.store.materialize_snapshot(None).await.unwrap(), model);
    assert!(
        db.store
            .list_node_agent_states(&system_admin())
            .await
            .unwrap()
            .nodes[0]
            .public_ip_change_notifications
    );
    let after = receipts(&db).await;
    db.store.migrate_prerelease_0007_schema().await.unwrap();
    assert_eq!(after, receipts(&db).await);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_notifications_compat_pg16() {
    check_notification_compat("16-alpine").await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_notifications_compat_pg17() {
    check_notification_compat("17-alpine").await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_notifications_compat_pg18() {
    check_notification_compat("18").await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_notifications_compat_keeps_clash_only_upgrade_available() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install_previous(&db, true).await;
    let model = db.store.materialize_snapshot(None).await.unwrap();
    let check = db.store.check_prerelease_0007_upgrade().await.unwrap();
    assert!(check.compatible, "{check:?}");
    db.store.migrate_prerelease_0007_schema().await.unwrap();
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    assert_eq!(db.store.materialize_snapshot(None).await.unwrap(), model);
    assert!(
        db.store
            .list_node_agent_states(&system_admin())
            .await
            .unwrap()
            .nodes[0]
            .public_ip_change_notifications
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_notifications_compat_rejects_unknown_or_partial_schemas_without_writes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install_previous(&db, false).await;
    sqlx::query("CREATE TABLE cvm_spec_artifacts(unexpected TEXT)")
        .execute(db.pool())
        .await
        .unwrap();
    let before = receipts(&db).await;
    assert!(
        db.store
            .check_prerelease_0007_upgrade()
            .await
            .unwrap()
            .compatible
    );
    assert!(db.store.migrate_prerelease_0007_schema().await.is_err());
    assert_eq!(before, receipts(&db).await);
    sqlx::query("DROP TABLE cvm_spec_artifacts")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE _sqlx_migrations SET checksum=decode(repeat('00',48),'hex') WHERE version=7",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let unknown = receipts(&db).await;
    assert!(
        !db.store
            .check_prerelease_0007_upgrade()
            .await
            .unwrap()
            .compatible
    );
    assert!(db.store.migrate_prerelease_0007_schema().await.is_err());
    assert_eq!(unknown, receipts(&db).await);
}
