use super::*;
use brocade_deployment::user_traffic::*;
const TENANT: &str = "platform.acme";

async fn seed_forwarding(db: &TestPg) -> i64 {
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO forwarding_entitlements(tenant_id,user_id,node_id,enabled,tcp_allowed,udp_allowed,listen_port_start,listen_port_end,max_rules,updated_by) VALUES($1,'alice','n1',true,true,true,30000,59999,100,'test')")
        .bind(TENANT).execute(db.pool()).await.unwrap();
    let rule = sqlx::query_scalar("INSERT INTO forwarding_rules(tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port,enabled) VALUES($1,'alice','n1','active','tcp',30001,'example.net',443,true) RETURNING id")
        .bind(TENANT).fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_rules(tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port,enabled) VALUES($1,'alice','n1','manual off','tcp',30002,'example.net',443,false)")
        .bind(TENANT).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_usage_rollups(period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap) SELECT starts_at,'n1',$1,'alice',$2,100,156,false FROM user_traffic_cycle(1,now())")
        .bind(TENANT).bind(rule).execute(db.pool()).await.unwrap();
    rule
}

fn budget(source: String, id: u8) -> UserBudgetRequest {
    UserBudgetRequest {
        source,
        request_id: format!("{id:032x}"),
        bytes: 1024,
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_total_is_unavailable_and_machine_quotas_remain_independent() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let rule = seed_forwarding(&db).await;
    let policy = UserTrafficPolicy {
        quota_bytes: Some("1".into()),
        exhausted_action: ExhaustedAction::Suspend,
    };
    assert!(matches!(
        db.store
            .update_user_traffic_policy(
                &system_admin(),
                TENANT,
                "alice",
                UserTrafficKind::Forwarding,
                None,
                policy
            )
            .await,
        Err(StoreError::Unsupported(_))
    ));
    assert!(db
        .store
        .user_traffic_policies(&system_admin(), TENANT, "alice")
        .await
        .unwrap()
        .iter()
        .all(|v| v.kind == UserTrafficKind::Access));
    assert!(db
        .store
        .user_traffic_assignments("n1")
        .await
        .unwrap()
        .sources
        .iter()
        .all(|v| !v.source.starts_with("fwd:")));
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", budget(format!("fwd:{rule}"), 1))
            .await
            .unwrap(),
        UserBudgetGrant::Unlimited { managed: false, .. }
    ));
    db.store
        .set_forwarding_quota(
            &system_admin(),
            TENANT,
            "alice",
            "n1",
            brocade_store::ForwardingQuotaWrite {
                limit_bytes: Some(128),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store
            .enforce_forwarding_quotas()
            .await
            .unwrap()
            .suspended,
        1
    );
    let view = db
        .store
        .forwarding_overview(&system_admin(), TENANT, "alice")
        .await
        .unwrap();
    assert_eq!(view.entitlements[0].cycle_limit_bytes, Some(128));
    assert!(view.entitlements[0].quota_suspended);
    assert!(db
        .store
        .forwarding_desired_for_node("n1")
        .await
        .unwrap()
        .rules
        .is_empty());
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", budget(format!("fwd:{rule}"), 2))
            .await
            .unwrap(),
        UserBudgetGrant::Suspended { .. }
    ));
    db.store
        .set_forwarding_quota(
            &system_admin(),
            TENANT,
            "alice",
            "n1",
            brocade_store::ForwardingQuotaWrite {
                limit_bytes: Some(1024),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store.enforce_forwarding_quotas().await.unwrap().restored,
        1
    );
    assert_eq!(
        db.store
            .forwarding_desired_for_node("n1")
            .await
            .unwrap()
            .rules
            .len(),
        1
    );
    let disabled: i64 =
        sqlx::query_scalar("SELECT count(*) FROM forwarding_rules WHERE NOT enabled")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(disabled, 1);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn removal_upgrade_archives_total_and_restores_machine_jobs_without_changing_access() {
    for with_target_strategy in [false, true] {
        removal_upgrade_from_deployed_history(with_target_strategy).await;
    }
}

async fn removal_upgrade_from_deployed_history(with_target_strategy: bool) {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let mut migrator = sqlx::migrate::Migrator::DEFAULT;
    migrator.migrations = std::borrow::Cow::Owned(vec![
        sqlx::migrate::Migration::new(
            1,
            "init".into(),
            sqlx::migrate::MigrationType::Simple,
            include_str!("../../migrations/0001_init.sql").into(),
            false,
        ),
        sqlx::migrate::Migration::new(
            2,
            "forwarding rule results".into(),
            sqlx::migrate::MigrationType::Simple,
            include_str!("../fixtures/migrations/deployed_0002_forwarding_rule_results.sql").into(),
            false,
        ),
    ]);
    if with_target_strategy {
        migrator
            .migrations
            .to_mut()
            .push(sqlx::migrate::Migration::new(
                3,
                "external outbound target strategy".into(),
                sqlx::migrate::MigrationType::Simple,
                include_str!(
                    "../fixtures/migrations/deployed_0003_external_outbound_target_strategy.sql"
                )
                .into(),
                false,
            ));
    }
    migrator.run(db.pool()).await.unwrap();
    let rule = seed_forwarding(&db).await;
    sqlx::query("INSERT INTO user_app_quotas(tenant_id,user_id,app_id,limit_bytes,exhausted_action,rate_bps,revision) VALUES($1,'alice','app-main',1024,'throttle',8000,7)")
        .bind(TENANT).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO user_traffic_policies(tenant_id,user_id,kind,quota_bytes,action,exhausted,suspended,updated_by) VALUES($1,'alice','forwarding',1,'suspend',true,true,'test')")
        .bind(TENANT).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_node_quotas(tenant_id,user_id,node_id,limit_bytes,suspended,updated_by) VALUES($1,'alice','n1',128,false,'test')")
        .bind(TENANT).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO nodes SELECT (jsonb_populate_record(NULL::nodes,to_jsonb(n)||'{\"id\":\"n2\",\"name\":\"Node 2\",\"public_ipv4\":\"n2.example.net\",\"overlay_addr\":\"10.66.0.2\",\"wg_public_key\":\"wg-public2\"}'::jsonb)).* FROM nodes n WHERE id='n1'")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_entitlements(tenant_id,user_id,node_id,enabled,tcp_allowed,udp_allowed,listen_port_start,listen_port_end,max_rules,updated_by) VALUES($1,'alice','n2',true,true,true,30000,59999,100,'test')")
        .bind(TENANT).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_node_state(node_id,desired_generation,reconcile_status) VALUES('n2',1,'pending')")
        .execute(db.pool()).await.unwrap();
    let old_job:i64=sqlx::query_scalar("INSERT INTO jobs(kind,status,payload) VALUES('forwarding-quota','queued',jsonb_build_object('tenant_id',$1::text,'user_id','alice','node_id','n2','generation',1,'suspended',false)) RETURNING id")
        .bind(TENANT).fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_node_quotas(tenant_id,user_id,node_id,limit_bytes,suspended,last_job_id,updated_by) VALUES($1,'alice','n2',1024,false,$2,'test')")
        .bind(TENANT).bind(old_job).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO user_traffic_budgets(tenant_id,user_id,kind,scope) VALUES($1,'alice','access','app-main'),($1,'alice','forwarding','')")
        .bind(TENANT).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO user_traffic_budget_receipts(tenant_id,user_id,kind,scope,node_id,request_id,bytes,rate_bps,expires_at) VALUES($1,'alice','forwarding','','n1','old',1,8000,now()+interval '1 hour')")
        .bind(TENANT).execute(db.pool()).await.unwrap();
    let access_before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(q) FROM user_app_quotas q")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let app_budget: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(q) FROM user_traffic_budgets q WHERE kind='access'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    assert!(db.store.migrate_database_schema().await.is_err());
    let receipts_before: Vec<(i64, serde_json::Value)> = sqlx::query_as(
        "SELECT version,to_jsonb(m)-'checksum' FROM _sqlx_migrations m WHERE version<=2 ORDER BY version",
    ).fetch_all(db.pool()).await.unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut command = std::process::Command::new("python3");
    command
        .arg(root.join("scripts/prepare-consolidated-0002-compat.py"))
        .arg("--migration")
        .arg(root.join("crates/brocade-store/migrations/0002_forwarding_rule_results.sql"))
        .args(["--output", "-"]);
    if !with_target_strategy {
        command.arg("--without-target-strategy");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let conversion = String::from_utf8(output.stdout)
        .unwrap()
        .replace("\\set ON_ERROR_STOP on", "")
        .replace("SET LOCAL ROLE :\"app_role\";", "SET LOCAL ROLE postgres;");
    let original: Vec<(i64, Vec<u8>)> = migrator
        .iter()
        .map(|m| (m.version, m.checksum.to_vec()))
        .collect();
    let mut connection = db.pool().acquire().await.unwrap();
    sqlx::query(
        "UPDATE _sqlx_migrations SET checksum=decode(repeat('00',48),'hex') WHERE version=2",
    )
    .execute(&mut *connection)
    .await
    .unwrap();
    assert!(sqlx::raw_sql(&conversion)
        .execute(&mut *connection)
        .await
        .is_err());
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
    let unknown: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=2")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
    assert_eq!(
        unknown,
        vec![0_u8; 48],
        "conversion must not repair an unknown receipt"
    );
    sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=2")
        .bind(&original[1].1)
        .execute(&mut *connection)
        .await
        .unwrap();
    // A failure after all DDL still rolls back the schema, data and original receipts together.
    let injected = conversion.replace(
        "UPDATE _sqlx_migrations SET checksum=",
        "DO $$ BEGIN RAISE EXCEPTION 'injected consolidation failure'; END $$;\nUPDATE _sqlx_migrations SET checksum=",
    );
    assert!(sqlx::raw_sql(&injected)
        .execute(&mut *connection)
        .await
        .is_err());
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
    let preserved: bool = sqlx::query_scalar(
        "SELECT to_regclass('user_traffic_policies') IS NOT NULL AND to_regclass('retired_forwarding_traffic_policies') IS NULL",
    ).fetch_one(&mut *connection).await.unwrap();
    assert!(preserved);
    let after_failure: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *connection)
            .await
            .unwrap();
    assert_eq!(after_failure, original);
    drop(connection);
    sqlx::raw_sql(&conversion).execute(db.pool()).await.unwrap();
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible && check.pending_versions.is_empty());
    assert_eq!(check.installed_versions, [1, 2]);
    let receipts_after: Vec<(i64, serde_json::Value)> = sqlx::query_as(
        "SELECT version,to_jsonb(m)-'checksum' FROM _sqlx_migrations m ORDER BY version",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(receipts_after, receipts_before);
    let mut connection = db.pool().acquire().await.unwrap();
    assert!(sqlx::raw_sql(&conversion)
        .execute(&mut *connection)
        .await
        .is_err());
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    db.store.migrate().await.unwrap();
    let access_after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(q) FROM user_app_quotas q")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(access_after, access_before);
    let after_budget: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(q) FROM user_traffic_budgets q WHERE kind='access'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(after_budget, app_budget);
    let state: (bool, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT suspended,limit_bytes,last_job_id FROM forwarding_node_quotas WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(state.0);
    assert_eq!(state.1, Some(128));
    let job: serde_json::Value = sqlx::query_scalar("SELECT payload FROM jobs WHERE id=$1")
        .bind(state.2.unwrap())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(job["suspended"], true);
    assert_eq!(job["used_bytes"], 256);
    let unchanged:(bool,i64,serde_json::Value)=sqlx::query_as("SELECT q.suspended,q.last_job_id,j.payload FROM forwarding_node_quotas q JOIN jobs j ON j.id=q.last_job_id WHERE q.node_id='n2'")
        .fetch_one(db.pool()).await.unwrap();
    assert!(!unchanged.0);
    assert_ne!(unchanged.1, old_job);
    assert_eq!(unchanged.2["generation"], 2);
    let old_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
        .bind(old_job)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(old_status, "canceled");
    let archived: i64 =
        sqlx::query_scalar("SELECT count(*) FROM retired_forwarding_traffic_policies")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(archived, 1);
    let removed:bool=sqlx::query_scalar("SELECT to_regclass('user_traffic_policies') IS NULL AND to_regprocedure('user_global_configured(text,text,text)') IS NULL AND NOT EXISTS(SELECT 1 FROM user_traffic_scopes WHERE kind='forwarding') AND NOT EXISTS(SELECT 1 FROM user_traffic_budgets WHERE kind='forwarding') AND NOT EXISTS(SELECT 1 FROM user_traffic_budget_receipts WHERE kind='forwarding')").fetch_one(db.pool()).await.unwrap();
    assert!(removed);
    assert!(db
        .store
        .user_traffic_assignments("n1")
        .await
        .unwrap()
        .sources
        .iter()
        .all(|row| row.source != format!("fwd:{rule}")));
    assert!(db
        .store
        .forwarding_desired_for_node("n1")
        .await
        .unwrap()
        .rules
        .is_empty());
}
