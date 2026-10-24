use super::*;
use brocade_deployment::source_access::*;

async fn setup(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
}
fn write(revision: i64, ips: &[&str]) -> SourceAccessWrite {
    SourceAccessWrite {
        revision,
        ips: ips.iter().map(|ip| ip.to_string()).collect(),
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and Docker PostgreSQL"]
async fn source_access_deleted_policy_invalidates_cached_snapshots_and_keeps_audit() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let view = db
        .store
        .update_source_access(
            &system_admin(),
            Some("n1"),
            None,
            None,
            write(0, &["192.0.2.1"]),
        )
        .await
        .unwrap();
    sqlx::query("DELETE FROM source_access_policies WHERE node_id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let desired = db.store.source_access_desired("n1").await.unwrap();
    assert!(desired.revision > view.revision);
    assert!(desired.node_ips.is_empty());
    let event: (String, serde_json::Value) =
        sqlx::query_as("SELECT actor,ips FROM source_access_events ORDER BY revision DESC LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        event,
        (
            "system:source-access-owner-deleted".into(),
            serde_json::json!([])
        )
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and Docker PostgreSQL"]
async fn source_access_normalizes_persists_combines_scopes_and_audits_without_release() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let model: i64 = sqlx::query_scalar("SELECT current_revision FROM control_state")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let user = db
        .store
        .update_source_access(
            &system_admin(),
            None,
            Some("platform.acme"),
            Some("alice"),
            write(0, &["::ffff:192.0.2.1", "192.0.2.1", "2001:0db8::1"]),
        )
        .await
        .unwrap();
    assert_eq!(user.ips, vec!["192.0.2.1", "2001:db8::1"]);
    let node = db
        .store
        .update_source_access(
            &system_admin(),
            Some("n1"),
            None,
            None,
            write(0, &["192.0.2.2"]),
        )
        .await
        .unwrap();
    let restarted = PgStore::from_pool(db.pool().clone());
    let desired = restarted.source_access_desired("n1").await.unwrap();
    assert!(desired.revision > user.revision);
    assert_eq!(desired.node_ips, node.ips);
    assert_eq!(desired.users.len(), 1);
    assert_eq!(desired.users[0].ips, user.ips);
    let cleared = db
        .store
        .update_source_access(
            &system_admin(),
            None,
            Some("platform.acme"),
            Some("alice"),
            write(user.revision, &[]),
        )
        .await
        .unwrap();
    assert!(cleared.revision > desired.revision);
    assert!(db
        .store
        .source_access_desired("n1")
        .await
        .unwrap()
        .users
        .is_empty());
    assert_eq!(
        db.store.source_access_desired("n1").await.unwrap().node_ips,
        vec!["192.0.2.2"]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM source_access_events")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        model
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM _sqlx_migrations WHERE version NOT IN (1,2)"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and Docker PostgreSQL"]
async fn source_access_concurrent_edits_conflict_and_unchanged_lists_do_not_create_events() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let actor = system_admin();
    let (a, b) = tokio::join!(
        db.store
            .update_source_access(&actor, Some("n1"), None, None, write(0, &["192.0.2.1"])),
        db.store
            .update_source_access(&actor, Some("n1"), None, None, write(0, &["192.0.2.2"]))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(
        a.as_ref().err().or(b.as_ref().err()),
        Some(StoreError::Conflict(_))
    ));
    let result = a.or(b).unwrap();
    db.store
        .update_source_access(
            &actor,
            Some("n1"),
            None,
            None,
            SourceAccessWrite {
                revision: result.revision,
                ips: result.ips,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM source_access_events")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and Docker PostgreSQL"]
async fn source_access_rejects_cross_tenant_node_edits_and_invalid_addresses() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let visitor = AdminContext::new(brocade_store::PUBLIC_OPERATOR_ID, AdminRole::Readonly, None);
    assert!(matches!(
        db.store
            .source_access_view(&visitor, None, Some("platform.acme"), Some("alice"))
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let reader = AdminContext::new(
        "source-reader",
        AdminRole::Readonly,
        Some("platform.acme".into()),
    );
    assert!(db
        .store
        .source_access_view(&reader, None, Some("platform.acme"), Some("alice"))
        .await
        .is_ok());
    assert!(matches!(
        db.store
            .update_source_access(
                &reader,
                None,
                Some("platform.acme"),
                Some("alice"),
                write(0, &["192.0.2.1"])
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store
            .source_access_view(
                &ordinary_user("platform.acme"),
                None,
                Some("platform.acme"),
                Some("alice")
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    for request in [
        write(0, &["example.com"]),
        write(0, &["192.0.2.0/24"]),
        write(0, &["fe80::1%eth0"]),
        SourceAccessWrite {
            revision: 0,
            ips: vec!["192.0.2.1".into(); 257],
        },
    ] {
        assert!(matches!(
            db.store
                .update_source_access(&system_admin(), Some("n1"), None, None, request)
                .await,
            Err(StoreError::InvalidData(_))
        ));
    }
    assert!(matches!(
        db.store
            .update_source_access(
                &tenant_admin("other"),
                None,
                Some("platform.acme"),
                Some("alice"),
                write(0, &["192.0.2.1"])
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store
            .update_source_access(
                &tenant_admin("platform.acme"),
                Some("n1"),
                None,
                None,
                write(0, &["192.0.2.1"])
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store
            .source_access_view(
                &tenant_admin("other"),
                None,
                Some("platform.acme"),
                Some("alice")
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM source_access_events")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and Docker PostgreSQL"]
async fn source_access_receipts_cannot_confirm_old_failed_or_unsupported_generations() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    sqlx::query("INSERT INTO node_agent_state(node_id,agent_protocol_version) VALUES('n1',45) ON CONFLICT(node_id) DO UPDATE SET agent_protocol_version=45").execute(db.pool()).await.unwrap();
    let policy = db
        .store
        .update_source_access(
            &system_admin(),
            Some("n1"),
            None,
            None,
            write(0, &["192.0.2.1"]),
        )
        .await
        .unwrap();
    for (revision, supported, error, phase) in [
        (0, true, None, SourceAccessPhase::Pending),
        (policy.revision, false, None, SourceAccessPhase::Unsupported),
        (
            policy.revision,
            true,
            Some("awaiting generation".into()),
            SourceAccessPhase::Error,
        ),
        (policy.revision, true, None, SourceAccessPhase::Applied),
    ] {
        db.store
            .report_source_access(
                "n1",
                SourceAccessObservation {
                    revision,
                    supported,
                    error,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            db.store
                .source_access_view(&system_admin(), Some("n1"), None, None)
                .await
                .unwrap()
                .nodes[0]
                .phase,
            phase
        );
    }
    db.store
        .report_source_access(
            "n1",
            SourceAccessObservation {
                revision: 0,
                supported: false,
                error: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store
            .source_access_view(&system_admin(), Some("n1"), None, None)
            .await
            .unwrap()
            .nodes[0]
            .phase,
        SourceAccessPhase::Applied
    );
    sqlx::query("UPDATE source_access_node_state SET reported_at=now()-interval '1 minute'")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        db.store
            .source_access_view(&system_admin(), Some("n1"), None, None)
            .await
            .unwrap()
            .nodes[0]
            .phase,
        SourceAccessPhase::Pending
    );
}
