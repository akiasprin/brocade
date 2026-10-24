use super::*;
use brocade_store::{ForwardingEntitlementWrite, ForwardingRuleWrite};

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_target_policy_revokes_old_private_rules_and_preserves_public_ips() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let policy = |allow_private| {
        serde_json::from_value::<ForwardingEntitlementWrite>(json!({
            "enabled":true,"allow_private_targets":allow_private
        }))
        .unwrap()
    };
    db.store
        .set_forwarding_entitlement(
            &system_admin(),
            "platform.acme",
            "alice",
            "n1",
            policy(true),
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    for (port, host) in [
        (40001, "1.1.1.1"),
        (40002, "10.1.2.3"),
        (40003, "localhost"),
    ] {
        let rule: ForwardingRuleWrite = serde_json::from_value(json!({"node_id":"n1","name":host,
            "protocol":"tcp","listen_port":port,"target_host":host,"target_port":443,"enabled":true})).unwrap();
        let overview = db
            .store
            .create_forwarding_rule(&system_admin(), "platform.acme", "alice", rule)
            .await
            .unwrap();
        ids.push(
            overview
                .rules
                .iter()
                .find(|rule| rule.listen_port == port)
                .unwrap()
                .id,
        );
    }
    let denied =
        serde_json::from_value(json!({"node_id":"n1","name":"management","protocol":"tcp",
        "listen_port":40004,"target_host":"127.0.0.1","target_port":10085,"enabled":true}))
        .unwrap();
    assert!(matches!(
        db.store
            .create_forwarding_rule(&system_admin(), "platform.acme", "alice", denied)
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let view = db
        .store
        .set_forwarding_entitlement(
            &system_admin(),
            "platform.acme",
            "alice",
            "n1",
            policy(false),
        )
        .await
        .unwrap();
    assert!(
        view.rules
            .iter()
            .find(|rule| rule.id == ids[0])
            .unwrap()
            .enabled
    );
    assert!(
        !view
            .rules
            .iter()
            .find(|rule| rule.id == ids[1])
            .unwrap()
            .enabled
    );
    let desired = db.store.forwarding_desired_for_node("n1").await.unwrap();
    assert_eq!(
        desired.rules.iter().map(|rule| rule.id).collect::<Vec<_>>(),
        [ids[0], ids[2]]
    );
    assert!(desired
        .rules
        .iter()
        .all(|rule| rule.allow_private_targets == Some(false)));
    // A legacy unsafe row must also be retired on the next desired-state read, even if its
    // entitlement was never edited after this Console upgrade.
    sqlx::query("UPDATE forwarding_rules SET enabled=true WHERE id=$1")
        .bind(ids[1])
        .execute(db.pool())
        .await
        .unwrap();
    let desired = db.store.forwarding_desired_for_node("n1").await.unwrap();
    assert!(!desired.rules.iter().any(|rule| rule.id == ids[1]));
    assert!(
        !db.store
            .forwarding_overview(&system_admin(), "platform.acme", "alice")
            .await
            .unwrap()
            .rules
            .iter()
            .find(|rule| rule.id == ids[1])
            .unwrap()
            .enabled
    );
}
