use super::*;
use brocade_core::format::yaml::{CustomRule, RuleKind, RulePreferences};
use brocade_store::clash_rules::Write;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_rules_save_without_revision_and_only_change_standard_clash() {
    let db = TestPg::start_if_enabled()
        .await
        .expect("explicit PostgreSQL test required");
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let revision = seed_subscription_serving(&db).await;
    let admin = system_admin();
    let tenant = "platform.acme";
    let user = "alice";
    let uuid = "2d2304da-f114-4574-8d44-625afdb1db5c";
    let before = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    let link = db
        .store
        .issue_clash_haitun_link(&admin, tenant, user)
        .await
        .unwrap();
    let speed_before = db
        .store
        .clash_subscription_by_haitun_token_for_family(&link.token, None)
        .await
        .unwrap();
    let view = db.store.clash_rules(&admin, tenant, user).await.unwrap();
    assert_eq!(view.version, 0);
    let mut rules = RulePreferences::default();
    rules.custom.push(CustomRule {
        id: "one".into(),
        kind: RuleKind::Domain,
        value: "example.invalid".into(),
        target: "direct".into(),
    });
    let saved = db
        .store
        .save_clash_rules(
            &admin,
            tenant,
            user,
            Write {
                version: 0,
                rules: rules.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.version, 1);
    assert_eq!(
        db.store
            .clash_rules(&admin, tenant, user)
            .await
            .unwrap()
            .rules,
        rules
    );
    let after = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert_eq!(after.revision, revision);
    assert_ne!(after.content, before.content);
    assert!(after
        .content
        .contains("DOMAIN-SUFFIX,example.invalid,DIRECT"));
    assert_eq!(
        speed_before.content,
        db.store
            .clash_subscription_by_haitun_token_for_family(&link.token, None)
            .await
            .unwrap()
            .content
    );
    assert!(matches!(
        db.store
            .save_clash_rules(
                &admin,
                tenant,
                user,
                Write {
                    version: 0,
                    rules: rules.clone()
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    let (a, b) = tokio::join!(
        db.store.save_clash_rules(
            &admin,
            tenant,
            user,
            Write {
                version: 1,
                rules: rules.clone()
            }
        ),
        db.store.save_clash_rules(
            &admin,
            tenant,
            user,
            Write {
                version: 1,
                rules: rules.clone()
            }
        )
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    for role in [AdminRole::Readonly, AdminRole::Publisher] {
        let actor = AdminContext::new("operator", role, None);
        assert!(matches!(
            db.store
                .save_clash_rules(
                    &actor,
                    tenant,
                    user,
                    Write {
                        version: 2,
                        rules: rules.clone()
                    }
                )
                .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    assert!(matches!(
        db.store
            .clash_rules(&tenant_admin("platform.other"), tenant, user)
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let self_actor = AdminContext::from_authenticated(&brocade_store::AuthenticatedAdmin {
        operator_id: "self".into(),
        role: AdminRole::User,
        tenant_scope: Some(tenant.into()),
        token_prefix: None,
        self_user: Some(brocade_store::AuthenticatedUser {
            tenant_id: tenant.into(),
            user_id: user.into(),
        }),
    });
    assert!(db
        .store
        .clash_rules(&self_actor, tenant, user)
        .await
        .is_ok());
    assert!(matches!(
        db.store.clash_rules(&self_actor, tenant, "bob").await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(db
        .store
        .save_clash_rules(
            &self_actor,
            tenant,
            user,
            Write {
                version: 2,
                rules: rules.clone()
            }
        )
        .await
        .is_ok());
    assert!(matches!(
        db.store
            .clash_rules(
                &AdminContext::new(brocade_store::PUBLIC_OPERATOR_ID, AdminRole::Readonly, None),
                tenant,
                user
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let query = db
        .store
        .preview_clash_rules(&admin, tenant, user, &rules)
        .await
        .unwrap();
    assert!(query.starts_with("proxy-groups:"));
    assert!(!query.contains(uuid));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn v037_rules_upgrade_preserves_receipts_and_connectors_and_retries_atomically() {
    let db = TestPg::start_if_enabled()
        .await
        .expect("explicit PostgreSQL test required");
    let mut released = sqlx::migrate::Migrator::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
    )
    .await
    .unwrap();
    released.migrations = std::borrow::Cow::Owned(
        released
            .iter()
            .filter(|m| m.version <= 6)
            .cloned()
            .collect(),
    );
    released.run(db.pool()).await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO subscription_connectors(tenant_id,user_id,metadata,remote_name) VALUES('platform.acme','alice','{\"title\":\"preserve\"}','fixture')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO subscription_connector_runs(tenant_id,user_id,config_version,kind,status,trigger,actor) VALUES('platform.acme','alice',1,'inspect','succeeded','manual','fixture')")
        .execute(db.pool()).await.unwrap();
    let preserved = "SELECT jsonb_build_object(
        'connectors',(SELECT jsonb_agg(to_jsonb(c)) FROM subscription_connectors c),
        'runs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM subscription_connector_runs r),
        'receipts',(SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m WHERE version<=6))";
    let before: serde_json::Value = sqlx::query_scalar(preserved)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible);
    assert_eq!(check.installed_versions, [1, 2, 3, 4, 5, 6]);
    assert_eq!(check.pending_versions, [7]);
    let absent: bool = sqlx::query_scalar("SELECT to_regclass('public.user_clash_rules') IS NULL")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(absent, "read-only preflight must not apply 0007");

    // A failed 0007 must leave the v0.3.7 database and receipts intact for a retry.
    sqlx::query("CREATE TABLE user_clash_rules(conflict TEXT)")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db.store.migrate_database_schema().await.is_err());
    let failed: serde_json::Value = sqlx::query_scalar(preserved)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(failed, before);
    let receipts: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(receipts, [1, 2, 3, 4, 5, 6]);
    sqlx::query("DROP TABLE user_clash_rules")
        .execute(db.pool())
        .await
        .unwrap();

    db.store.migrate_database_schema().await.unwrap();
    let after: serde_json::Value = sqlx::query_scalar(preserved)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(after, before);
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible && check.pending_versions.is_empty());
    assert_eq!(check.installed_versions, [1, 2, 3, 4, 5, 6, 7]);
    sqlx::query("INSERT INTO user_clash_rules(tenant_id,user_id,version,rules) VALUES('platform.acme','alice',3,'{\"custom\":[]}')")
        .execute(db.pool()).await.unwrap();
    let rules_and_receipt = "SELECT jsonb_build_object('rules',(SELECT to_jsonb(r) FROM user_clash_rules r),'receipt',(SELECT to_jsonb(m) FROM _sqlx_migrations m WHERE version=7))";
    let saved: serde_json::Value = sqlx::query_scalar(rules_and_receipt)
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.store.migrate_database_schema().await.unwrap();
    let repeated: serde_json::Value = sqlx::query_scalar(rules_and_receipt)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        repeated, saved,
        "restarting must retain saved rules and the 0007 receipt"
    );
}
