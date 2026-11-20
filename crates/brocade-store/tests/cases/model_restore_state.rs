use super::*;

async fn fixture() -> Option<(TestPg, i64)> {
    fixture_with_udp(false).await
}

async fn fixture_with_udp(udp: bool) -> Option<(TestPg, i64)> {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let db = TestPg::start_if_enabled().await?;
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO ingresses(id,app_id,chain_id,node_id,bind,port,transport_kind,reality_private_key,reality_public_key,reality_short_ids,reality_dest,reality_server_names,reality_flow,reality_fallback_mode)
        SELECT 'ing-secondary',app_id,chain_id,node_id,bind,444,transport_kind,reality_private_key||'-secondary',reality_public_key||'-secondary',reality_short_ids,reality_dest,reality_server_names,reality_flow,reality_fallback_mode FROM ingresses WHERE id='ing-a1b2'")
        .execute(db.pool()).await.unwrap();
    if udp {
        sqlx::raw_sql("INSERT INTO cert_domains(id,domain,acme_directory) VALUES('restore-cert-domain','restore.example.net','https://acme.test/directory');
            INSERT INTO cert_labels(id,domain_id,label,name) VALUES('restore-cert-group','restore-cert-domain','a1b2c3d4','Default');
            INSERT INTO certificates(id,label_id,cert_pem,key_pem_sealed,issued_at,expires_at,status) VALUES('restore-cert','restore-cert-group','cert','sealed',now(),now()+interval '30 days','serving');
            INSERT INTO node_cert_label(node_id,label_id) VALUES('n1','restore-cert-group');
            UPDATE ingresses SET hy2_enabled=true,hy2_port=CASE id WHEN 'ing-a1b2' THEN 7443 ELSE 7444 END,
                anytls_enabled=true,anytls_port=CASE id WHEN 'ing-a1b2' THEN 1 ELSE 2 END;")
            .execute(db.pool()).await.unwrap();
    }
    store_current_model_snapshot(db.pool(), &db.store).await;
    let revision = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: UpsertExternalOutboundRequest {
                    id: "custom-1111-1111".into(),
                    tenant_id: "platform.acme".into(),
                    name: "Retained tunnel".into(),
                    address: "proxy.example.net".into(),
                    port: 1080,
                    protocol: ExternalOutboundProtocol::Socks5 {
                        username: None,
                        credential: String::new(),
                    },
                    security: ExternalOutboundSecurity::None,
                    target_strategy: None,
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap()
        .revision_id;
    let deployment = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(revision, "restore-state-base"),
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(desired.deployment_id, deployment.deployment_id);
    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO user_app_rate_limits(tenant_id,user_id,app_id,base_rate_bps) VALUES('platform.acme','alice','app-main',2000000);
        INSERT INTO user_online_sources(node_id,tenant_id,user_id,ingress_id,source_ip,active,first_observed_at,last_observed_at,xray_last_seen_at,offline_at,protocols,online_since_at,session_uplink_bytes,session_downlink_bytes)
        VALUES('n1','platform.acme','alice','ing-a1b2','1.1.1.1',true,now()-interval '1 hour',now(),now(),NULL,'[\"vless\"]',now()-interval '1 hour',123,456),
        ('n1','platform.acme','alice','ing-a1b2','2606:4700::1111',false,now()-interval '2 hours',now()-interval '1 hour',now()-interval '1 hour',now()-interval '1 hour','[\"anytls\"]',now()-interval '2 hours',789,1024);
        INSERT INTO unlock_probe_jobs(scope_key,node_id,app_id,chain_id,configuration_revision,protocol,job_id,state,trigger,address_family,completed_at,results)
        VALUES('chain:app-main/chn-a1b2-c3d4:v4','n1','app-main','chn-a1b2-c3d4',1,'vless','restore-state-unlock','complete','scheduled','v4',now(),'[]');
        INSERT INTO external_outbound_probe_policies(outbound_id,enabled,interval_secs,timeout_secs,next_run_at,updated_by)
        VALUES('custom-1111-1111',true,900,20,now()+interval '15 minutes','tester');")
        .execute(db.pool()).await.unwrap();
    Some((db, deployment.deployment_id))
}

async fn operational_state(db: &TestPg) -> serde_json::Value {
    sqlx::query_scalar("SELECT jsonb_build_object(
        'sources',(SELECT jsonb_agg(to_jsonb(s) ORDER BY session_id) FROM user_online_sources s),
        'limits',(SELECT jsonb_agg(to_jsonb(s)) FROM user_app_rate_limits s),
        'unlock',(SELECT jsonb_agg(to_jsonb(s)) FROM unlock_probe_jobs s),
        'tunnel_schedule',(SELECT jsonb_agg(to_jsonb(s)) FROM external_outbound_probe_policies s),
        'ingress_created',(SELECT jsonb_build_array(created_at,created_revision) FROM ingresses WHERE id='ing-a1b2'))")
        .fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn model_restore_state_discard_preserves_independent_data() {
    let Some((db, _)) = fixture().await else {
        return;
    };
    let before = operational_state(&db).await;
    let changed = db
        .store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                mtu: Some(1360),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.store
        .discard_pending_changes(&system_admin(), changed.revision_id)
        .await
        .unwrap();
    assert_eq!(operational_state(&db).await, before);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn model_restore_state_rollback_preserves_independent_data() {
    let Some((db, base)) = fixture().await else {
        return;
    };
    let before = operational_state(&db).await;
    db.store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                mtu: Some(1360),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.store
        .create_rollback_deployment(
            &system_admin(),
            create_rollback_request(base, "restore-state-rollback"),
        )
        .await
        .unwrap();
    assert_eq!(operational_state(&db).await, before);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn model_restore_state_discards_port_swaps_without_replacing_ingresses() {
    let Some((db, _)) = fixture().await else {
        return;
    };
    let before = operational_state(&db).await;
    let revision = insert_revision(db.pool(), "pending port swap").await;
    sqlx::raw_sql(
        "UPDATE ingresses SET port=445 WHERE id='ing-a1b2';
        UPDATE ingresses SET port=443 WHERE id='ing-secondary';
        UPDATE ingresses SET port=444 WHERE id='ing-a1b2';",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE control_state SET current_revision=$1 WHERE id")
        .bind(revision)
        .execute(db.pool())
        .await
        .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;
    db.store
        .discard_pending_changes(&system_admin(), revision as u64)
        .await
        .unwrap();
    let ports: Vec<i32> = sqlx::query_scalar("SELECT port FROM ingresses ORDER BY id")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(ports, vec![443, 444]);
    let temporary_wires: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ingresses WHERE mtproto_port IS NOT NULL OR transport_kind IS NULL",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(temporary_wires, 0);
    assert_eq!(operational_state(&db).await, before);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn model_restore_state_deletes_only_genuinely_removed_entities() {
    let Some((db, _)) = fixture().await else {
        return;
    };
    let before = operational_state(&db).await;
    let revision = insert_revision(db.pool(), "pending new app").await;
    sqlx::raw_sql("INSERT INTO apps(id,label,position) VALUES('app-extra','Extra',1);
        INSERT INTO chains(id,app_id,tenant_id,name,position) VALUES('chn-extra','app-extra','platform.acme','Extra',0);
        INSERT INTO ingresses(id,app_id,chain_id,node_id,bind,port,transport_kind,reality_private_key,reality_public_key,reality_short_ids,reality_dest,reality_server_names,reality_flow,reality_fallback_mode)
        SELECT 'ing-extra','app-extra','chn-extra',node_id,bind,445,transport_kind,reality_private_key||'-extra',reality_public_key||'-extra',reality_short_ids,reality_dest,reality_server_names,reality_flow,reality_fallback_mode FROM ingresses WHERE id='ing-a1b2';
        INSERT INTO user_app_rate_limits(tenant_id,user_id,app_id,base_rate_bps) VALUES('platform.acme','alice','app-extra',1000000);
        INSERT INTO user_online_sources(node_id,tenant_id,user_id,ingress_id,source_ip,active,first_observed_at,last_observed_at,xray_last_seen_at,offline_at)
        VALUES('n1','platform.acme','alice','ing-extra','8.8.8.8',false,now(),now(),now(),now());")
        .execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE control_state SET current_revision=$1 WHERE id")
        .bind(revision)
        .execute(db.pool())
        .await
        .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;
    db.store
        .discard_pending_changes(&system_admin(), revision as u64)
        .await
        .unwrap();
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT FROM apps WHERE id='app-extra')")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!exists);
    assert_eq!(operational_state(&db).await, before);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn model_restore_state_rolls_back_every_intermediate_change_on_failure() {
    let Some((db, _)) = fixture().await else {
        return;
    };
    let revision = db
        .store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                mtu: Some(1360),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .revision_id;
    let before = operational_state(&db).await;
    let model_before = db.store.materialize_snapshot(None).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_restored_ingress() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture interrupted ingress restore'; END; $$;
        CREATE TRIGGER reject_restored_ingress BEFORE UPDATE ON ingresses FOR EACH ROW WHEN(NEW.transport_kind IS NOT NULL) EXECUTE FUNCTION reject_restored_ingress();")
        .execute(db.pool()).await.unwrap();
    let failed = db
        .store
        .discard_pending_changes(&system_admin(), revision)
        .await;
    assert!(
        matches!(failed, Err(StoreError::Sqlx(_))),
        "the fixture must interrupt the actual restore write"
    );
    assert_eq!(
        db.store.materialize_snapshot(None).await.unwrap(),
        model_before
    );
    assert_eq!(operational_state(&db).await, before);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn model_restore_state_restores_udp_swaps_with_independent_tcp_wires() {
    let Some((db, _)) = fixture_with_udp(true).await else {
        return;
    };
    let before = operational_state(&db).await;
    let revision = insert_revision(db.pool(), "pending UDP port swap").await;
    sqlx::raw_sql(
        "UPDATE ingresses SET hy2_port=7445 WHERE id='ing-a1b2';
        UPDATE ingresses SET hy2_port=7443 WHERE id='ing-secondary';
        UPDATE ingresses SET hy2_port=7444 WHERE id='ing-a1b2';",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE control_state SET current_revision=$1 WHERE id")
        .bind(revision)
        .execute(db.pool())
        .await
        .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;
    db.store
        .discard_pending_changes(&system_admin(), revision as u64)
        .await
        .unwrap();
    let ports: Vec<i32> = sqlx::query_scalar("SELECT hy2_port FROM ingresses ORDER BY id")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(ports, vec![7443, 7444]);
    let restored_wires:i64=sqlx::query_scalar("SELECT count(*) FROM ingresses WHERE hy2_enabled AND transport_kind IS NOT NULL AND anytls_enabled AND mtproto_port IS NULL").fetch_one(db.pool()).await.unwrap();
    assert_eq!(restored_wires, 2);
    assert_eq!(operational_state(&db).await, before);
}
