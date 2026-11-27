use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_new_migration_deletes_legacy_cvm_data_and_preserves_non_cvm_state() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let mut prefix = sqlx::migrate::Migrator::new(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/migrations"
    )))
    .await
    .unwrap();
    prefix.migrations = prefix
        .iter()
        .filter(|m| m.version < 9)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    prefix.run(db.pool()).await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let historical: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(historical.len(), 8);
    let legacy = serde_json::json!({"id":"cvm-legacy","operation_id":"op-legacy","generation":1,"spec_sha256":"a".repeat(64),"power":"running","delete_data":false,"restart_epoch":0,"reinstall_epoch":0,
        "spec":{"name":"legacy","image":null,"resources":{"vcpus":1,"memory_bytes":134217728,"root_disk_bytes":3221225472u64},"ssh_public_key":"discarded-with-legacy-cvm","interfaces":[]}});
    sqlx::query("INSERT INTO cvm_instances(id,tenant_id,user_id,node_id,name,generation,desired) VALUES('cvm-legacy','platform.acme','alice','n1','legacy',1,$1)").bind(&legacy).execute(db.pool()).await.unwrap();
    let mut legacy_inventory = serde_json::to_value(catalogue()).unwrap();
    legacy_inventory
        .as_object_mut()
        .unwrap()
        .remove("chain_egress");
    sqlx::query("INSERT INTO cvm_hosts(node_id,capabilities) VALUES('n1',$1)")
        .bind(&legacy_inventory)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO cvm_quotas(tenant_id,user_id) VALUES('platform.acme','alice')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO cvm_user_host_network_quotas(tenant_id,user_id,node_id,id,revision,limits,held_states) VALUES('platform.acme','alice','n1','1111111111111111',1,'{}',100)").execute(db.pool()).await.unwrap();
    let revision: i64 =
        sqlx::query_scalar("INSERT INTO revisions(author) VALUES('migration-test') RETURNING id")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let deployment: i64 = sqlx::query_scalar(
        "INSERT INTO deployments(revision_id,status,active) VALUES($1,'planned',true) RETURNING id",
    )
    .bind(revision)
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO deployment_targets(deployment_id,node_id,status) VALUES($1,'n1','pending')",
    )
    .bind(deployment)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO deployment_target_state(
             deployment_id,node_id,wave,disruptive,desired_structure,observed_after,verdict,
             desired_grants,cvm_revision_id,cvm_machine_ready
         ) VALUES(
             $1,'n1',0,false,
             '{\"machine\":{\"kept\":true},\"cvm\":{\"legacy\":true}}',
             '{\"machine\":{\"kept\":true},\"cvm\":{\"legacy\":true}}',
             '{\"machine_status\":\"kept\",\"cvm_pending\":true,\"cvm_status\":\"pending\"}',
             '{}',$2,true
         )",
    )
    .bind(deployment)
    .bind(revision)
    .execute(db.pool())
    .await
    .unwrap();
    db.store.migrate().await.unwrap();
    db.store.migrate().await.unwrap();
    let after: Vec<(i64, Vec<u8>)> = sqlx::query_as(
        "SELECT version,checksum FROM _sqlx_migrations WHERE version<9 ORDER BY version",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(after, historical);
    let last: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(last, 9);
    let cvm_tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables
          WHERE schemaname='public' AND tablename LIKE 'cvm_%'
          ORDER BY tablename",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(cvm_tables.len(), 43);
    for table in cvm_tables {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "legacy rows remain in {table}");
    }
    let ordinary: (i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT count(*) FROM nodes),
             (SELECT count(*) FROM users),
             (SELECT count(*) FROM ingresses)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(ordinary, (1, 1, 1));
    let projection: (
        serde_json::Value,
        serde_json::Value,
        serde_json::Value,
        Option<i64>,
        bool,
    ) = sqlx::query_as(
        "SELECT desired_structure,observed_after,verdict,cvm_revision_id,cvm_machine_ready
               FROM deployment_target_state WHERE deployment_id=$1 AND node_id='n1'",
    )
    .bind(deployment)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(projection.0, serde_json::json!({"machine":{"kept":true}}));
    assert_eq!(projection.1, serde_json::json!({"machine":{"kept":true}}));
    assert_eq!(projection.2, serde_json::json!({"machine_status":"kept"}));
    assert_eq!(projection.3, None);
    assert!(!projection.4);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_admin_creates_and_grants_access_without_user_quotas_or_key_input() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let mut request = create_request("admin-unassigned-create-01");
    request.user_id = None;
    assert!(matches!(
        db.store
            .create_cvm(&actor_user("alice"), request.clone())
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let created = db
        .store
        .create_cvm(&system_admin(), request.clone())
        .await
        .unwrap();
    let replay = db.store.create_cvm(&system_admin(), request).await.unwrap();
    assert_eq!(created.desired.id, replay.desired.id);
    assert!(created.user_id.is_none());
    assert!(serde_json::to_value(&created.desired.spec)
        .unwrap()
        .get("ssh_public_key")
        .is_none());
    assert_eq!(created.desired.spec.login_revision, 1);
    assert!(db
        .store
        .cvm_workspace(&actor_user("alice"))
        .await
        .unwrap()
        .instances
        .is_empty());
    assert!(db
        .store
        .cvm_login_credential(&system_admin(), &created.desired.id)
        .await
        .is_err());
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&created))
        .await
        .unwrap();
    let original = db
        .store
        .cvm_login_credential(&system_admin(), &created.desired.id)
        .await
        .unwrap();
    assert!(original.validate().is_ok());
    let sealed: String = sqlx::query_scalar(
        "SELECT password_sealed FROM cvm_guest_credentials WHERE instance_id=$1 AND revision=1",
    )
    .bind(&created.desired.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(sealed.starts_with("v1.") && !sealed.contains(&original.password));
    assert!(!serde_json::to_string(&created)
        .unwrap()
        .contains(&original.password));
    assert!(db
        .store
        .cvm_node_credential("n2", &created.desired.id, 1)
        .await
        .is_err());
    supersede_cvm_fixture(&db).await;
    let assigned = db
        .store
        .command_cvm(
            &system_admin(),
            &created.desired.id,
            CvmCommand {
                request_id: "admin-assign-alice-01".into(),
                expected_generation: created.desired.generation,
                change: CvmChange::Assign {
                    user_id: Some("alice".into()),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(assigned.user_id.as_deref(), Some("alice"));
    assert!(db
        .store
        .cvm_login_credential(&actor_user("alice"), &created.desired.id)
        .await
        .is_err());
    assert!(db
        .store
        .cvm_node_credential("n1", &created.desired.id, 1)
        .await
        .is_err());
    assert!(db.store.cvm_desired("n1").await.is_err());
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&assigned))
        .await
        .unwrap();
    let credential = db
        .store
        .cvm_login_credential(&actor_user("alice"), &created.desired.id)
        .await
        .unwrap();
    assert!(credential.password != original.password);
    let user = actor_user("alice");
    let workspace = db.store.cvm_workspace(&user).await.unwrap();
    assert!(!workspace.can_write && workspace.can_use);
    assert!(db
        .store
        .cvm_login_credential(&actor_user("bob"), &created.desired.id)
        .await
        .is_err());
    for change in [
        CvmChange::ResetPassword,
        CvmChange::Delete { delete_data: true },
        CvmChange::Assign { user_id: None },
    ] {
        assert!(matches!(
            db.store
                .command_cvm(
                    &user,
                    &created.desired.id,
                    CvmCommand {
                        request_id: "ordinary-user-admin-denied-01".into(),
                        expected_generation: assigned.desired.generation,
                        change,
                    }
                )
                .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    supersede_cvm_fixture(&db).await;
    let revoked = db
        .store
        .command_cvm(
            &system_admin(),
            &created.desired.id,
            CvmCommand {
                request_id: "admin-revoke-alice-01".into(),
                expected_generation: assigned.desired.generation,
                change: CvmChange::Assign { user_id: None },
            },
        )
        .await
        .unwrap();
    assert!(revoked.user_id.is_none());
    assert_eq!(revoked.desired.power, PowerState::Stopped);
    assert!(db
        .store
        .cvm_instance(&user, &created.desired.id)
        .await
        .is_err());
    assert!(db
        .store
        .cvm_login_credential(&user, &created.desired.id)
        .await
        .is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM cvm_instance_access_events WHERE instance_id=$1"
        )
        .bind(&created.desired.id)
        .fetch_one(db.pool())
        .await
        .unwrap(),
        2
    );
    // A pending revocation cannot be bypassed by resetting the password first.
    assert!(matches!(
        db.store
            .command_cvm(
                &system_admin(),
                &created.desired.id,
                CvmCommand {
                    request_id: "admin-reset-after-revoke-01".into(),
                    expected_generation: revoked.desired.generation,
                    change: CvmChange::ResetPassword,
                }
            )
            .await,
        Err(StoreError::InvalidData(_))
    ));
    assert!(matches!(
        db.store
            .command_cvm(
                &system_admin(),
                &created.desired.id,
                CvmCommand {
                    request_id: "grant-before-revoke-applied-01".into(),
                    expected_generation: revoked.desired.generation,
                    change: CvmChange::Assign {
                        user_id: Some("alice".into())
                    },
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(
        matches!(db.store.command_cvm(&system_admin(), &created.desired.id, CvmCommand {
        request_id: "undo-must-not-restore-alice-01".into(),
        expected_generation: revoked.desired.generation,
        change: CvmChange::UndoPending { expected_revision: revoked.revision_id.unwrap() },
    }).await, Err(StoreError::Conflict(message)) if message.contains("使用授权"))
    );
    publish_cvm_intent(&db).await;
    let mut stopped = evidence(&revoked);
    stopped.runtime_id = None;
    stopped.interfaces.clear();
    stopped.network = None;
    stopped.network_absent = true;
    stopped.volume_retained = true;
    db.store.cvm_record_report("n1", stopped).await.unwrap();
    assert!(db
        .store
        .cvm_login_credential(&system_admin(), &created.desired.id)
        .await
        .is_err());
    supersede_cvm_fixture(&db).await;
    let reassigned = db
        .store
        .command_cvm(
            &system_admin(),
            &created.desired.id,
            CvmCommand {
                request_id: "admin-reassign-alice-01".into(),
                expected_generation: revoked.desired.generation,
                change: CvmChange::Assign {
                    user_id: Some("alice".into()),
                },
            },
        )
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    let mut stopped = evidence(&reassigned);
    stopped.runtime_id = None;
    stopped.interfaces.clear();
    stopped.network = None;
    stopped.network_absent = true;
    stopped.volume_retained = true;
    db.store.cvm_record_report("n1", stopped).await.unwrap();
    supersede_cvm_fixture(&db).await;
    let started = db
        .store
        .command_cvm(
            &user,
            &created.desired.id,
            CvmCommand {
                request_id: "user-start-reassigned-01".into(),
                expected_generation: reassigned.desired.generation,
                change: CvmChange::Start,
            },
        )
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&started))
        .await
        .unwrap();
    assert!(db
        .store
        .cvm_login_credential(&user, &created.desired.id)
        .await
        .is_ok());
}
