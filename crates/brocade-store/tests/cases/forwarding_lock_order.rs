use super::*;
use brocade_store::{ForwardingEntitlementWrite, ForwardingQuotaWrite, ForwardingRuleWrite};

const POLICY_GATE: i64 = 774_499;
const GENERATION_GATE: i64 = 774_500;

fn entitlement(enabled: bool, allow_private_targets: bool) -> ForwardingEntitlementWrite {
    serde_json::from_value(json!({
        "enabled": enabled, "allow_private_targets": allow_private_targets
    }))
    .unwrap()
}

async fn fixture(db: &TestPg) -> (i64, ForwardingRuleWrite) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store
        .set_forwarding_entitlement(
            &system_admin(),
            "platform.acme",
            "alice",
            "n1",
            entitlement(true, true),
        )
        .await
        .unwrap();
    let write: ForwardingRuleWrite = serde_json::from_value(json!({
        "node_id":"n1", "name":"Lock order fixture", "protocol":"tcp",
        "listen_port":40001, "target_host":"1.1.1.1", "target_port":443, "enabled":true
    }))
    .unwrap();
    let created = db
        .store
        .create_forwarding_rule(&system_admin(), "platform.acme", "alice", write.clone())
        .await
        .unwrap();
    (created.rules[0].id, write)
}

async fn wait_for_locked_queries(pool: &PgPool, pattern: &str, minimum: i64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity
                  WHERE datname = current_database() AND pid <> pg_backend_pid()
                    AND wait_event_type = 'Lock' AND query LIKE $1",
            )
            .bind(pattern)
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting >= minimum {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("concurrent forwarding operations must reach their lock wait");
}

async fn generation_gate(pool: &PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    sqlx::query(
        "CREATE FUNCTION forwarding_generation_gate() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_advisory_xact_lock(774500); RETURN NEW; END $$",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER forwarding_generation_gate AFTER UPDATE ON forwarding_node_state
         FOR EACH ROW EXECUTE FUNCTION forwarding_generation_gate()",
    )
    .execute(pool)
    .await
    .unwrap();
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(GENERATION_GATE)
        .execute(&mut *gate)
        .await
        .unwrap();
    gate
}

async fn edit_policy_while_updating_rule(enabled: bool) {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (rule_id, mut write) = fixture(&db).await;
    // Pause the real policy API after it owns the entitlement lock, before it locks rules.
    sqlx::query(
        "CREATE FUNCTION forwarding_policy_gate() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_advisory_xact_lock(774499); RETURN NEW; END $$",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER forwarding_policy_gate BEFORE UPDATE ON forwarding_entitlements
         FOR EACH ROW EXECUTE FUNCTION forwarding_policy_gate()",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let mut gate = db.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(POLICY_GATE)
        .execute(&mut *gate)
        .await
        .unwrap();
    let store = db.store.clone();
    let policy = tokio::spawn(async move {
        store
            .set_forwarding_entitlement(
                &system_admin(),
                "platform.acme",
                "alice",
                "n1",
                entitlement(enabled, false),
            )
            .await
    });
    wait_for_locked_queries(db.pool(), "INSERT INTO forwarding_entitlements%", 1).await;
    write.target_port = 8443;
    let store = db.store.clone();
    let edit = tokio::spawn(async move {
        store
            .update_forwarding_rule(&system_admin(), "platform.acme", "alice", rule_id, write)
            .await
    });
    wait_for_locked_queries(db.pool(), "%FROM forwarding_entitlements%FOR UPDATE%", 1).await;
    gate.commit().await.unwrap();

    let (policy, edit) = tokio::time::timeout(Duration::from_secs(5), async {
        (policy.await.unwrap(), edit.await.unwrap())
    })
    .await
    .expect("policy and rule edits must finish without a deadlock");
    policy.expect("policy changes must commit even with a concurrent rule edit");
    let desired = db.store.forwarding_desired_for_node("n1").await.unwrap();
    if enabled {
        edit.expect("a valid public rule edit must wait for the policy and then succeed");
        assert_eq!(desired.rules.len(), 1);
        assert_eq!(desired.rules[0].target_port, 8443);
        assert_eq!(desired.rules[0].allow_private_targets, Some(false));
    } else {
        assert!(matches!(edit, Err(StoreError::Forbidden(_))));
        assert!(desired.rules.is_empty());
        let target_port: i32 =
            sqlx::query_scalar("SELECT target_port FROM forwarding_rules WHERE id=$1")
                .bind(rule_id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(target_port, 443, "a rejected edit must not change the rule");
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_policy_edits_and_rule_updates_follow_one_lock_order() {
    edit_policy_while_updating_rule(true).await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_revocation_commits_before_a_waiting_rule_update_is_validated() {
    edit_policy_while_updating_rule(false).await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_opposite_rule_moves_lock_node_generations_in_one_order() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (first_id, mut first) = fixture(&db).await;
    insert_basic_node(db.pool(), "n2", "platform.acme", "10.66.0.2", 51821).await;
    db.store
        .set_forwarding_entitlement(
            &system_admin(),
            "platform.acme",
            "alice",
            "n2",
            entitlement(true, true),
        )
        .await
        .unwrap();
    let mut second = first.clone();
    second.node_id = "n2".to_owned();
    second.listen_port = 40002;
    let created = db
        .store
        .create_forwarding_rule(&system_admin(), "platform.acme", "alice", second.clone())
        .await
        .unwrap();
    let second_id = created
        .rules
        .iter()
        .find(|rule| rule.listen_port == 40002)
        .unwrap()
        .id;
    // The first mover holds its first node state while the opposite mover starts. If each
    // takes its old node first, they own opposite states and deadlock after this gate opens.
    let gate = generation_gate(db.pool()).await;
    first.node_id = "n2".to_owned();
    let store = db.store.clone();
    let first_move = tokio::spawn(async move {
        store
            .update_forwarding_rule(&system_admin(), "platform.acme", "alice", first_id, first)
            .await
    });
    wait_for_locked_queries(db.pool(), "INSERT INTO forwarding_node_state%", 1).await;
    second.node_id = "n1".to_owned();
    let store = db.store.clone();
    let second_move = tokio::spawn(async move {
        store
            .update_forwarding_rule(&system_admin(), "platform.acme", "alice", second_id, second)
            .await
    });
    wait_for_locked_queries(db.pool(), "INSERT INTO forwarding_node_state%", 2).await;
    gate.commit().await.unwrap();
    let (first_move, second_move) = tokio::time::timeout(Duration::from_secs(5), async {
        (first_move.await.unwrap(), second_move.await.unwrap())
    })
    .await
    .expect("opposite rule moves must finish without a deadlock");
    first_move.unwrap();
    second_move.unwrap();
    for (node, expected_id) in [("n1", second_id), ("n2", first_id)] {
        let desired = db.store.forwarding_desired_for_node(node).await.unwrap();
        assert_eq!(desired.rules.len(), 1);
        assert_eq!(desired.rules[0].id, expected_id);
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_quota_batches_and_rule_moves_lock_node_generations_in_one_order() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (rule_id, mut write) = fixture(&db).await;
    insert_basic_node(db.pool(), "n2", "platform.acme", "10.66.0.2", 51821).await;
    sqlx::query(
        "INSERT INTO users(tenant_id,id,uuid)
         VALUES('platform.acme','bob','20111111-1111-4111-8111-111111111111')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    // User ordering visits n2 before n1. The quota worker must use node ordering instead.
    for (user, node) in [("alice", "n2"), ("bob", "n1")] {
        db.store
            .set_forwarding_entitlement(
                &system_admin(),
                "platform.acme",
                user,
                node,
                entitlement(true, true),
            )
            .await
            .unwrap();
        db.store
            .set_forwarding_quota(
                &system_admin(),
                "platform.acme",
                user,
                node,
                ForwardingQuotaWrite { limit_bytes: None },
            )
            .await
            .unwrap();
    }
    // Model two suspended quotas whose limits were cleared, so the worker must restore both.
    sqlx::query("UPDATE forwarding_node_quotas SET suspended=true")
        .execute(db.pool())
        .await
        .unwrap();
    let gate = generation_gate(db.pool()).await;
    write.node_id = "n2".to_owned();
    let store = db.store.clone();
    let move_rule = tokio::spawn(async move {
        store
            .update_forwarding_rule(&system_admin(), "platform.acme", "alice", rule_id, write)
            .await
    });
    wait_for_locked_queries(db.pool(), "INSERT INTO forwarding_node_state%", 1).await;
    let store = db.store.clone();
    let quotas = tokio::spawn(async move { store.enforce_forwarding_quotas().await });
    wait_for_locked_queries(db.pool(), "INSERT INTO forwarding_node_state%", 2).await;
    gate.commit().await.unwrap();
    let (move_rule, quotas) = tokio::time::timeout(Duration::from_secs(5), async {
        (move_rule.await.unwrap(), quotas.await.unwrap())
    })
    .await
    .expect("rule moves and quota batches must finish without a deadlock");
    move_rule.unwrap();
    assert_eq!(quotas.unwrap().restored, 2);
    assert!(db
        .store
        .forwarding_desired_for_node("n1")
        .await
        .unwrap()
        .rules
        .is_empty());
    let desired = db.store.forwarding_desired_for_node("n2").await.unwrap();
    assert_eq!(desired.rules.len(), 1);
    assert_eq!(desired.rules[0].id, rule_id);
}
