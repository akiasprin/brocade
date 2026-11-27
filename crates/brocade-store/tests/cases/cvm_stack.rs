use super::*;
use brocade_deployment::cvm::stack::*;
use brocade_store::cvm::stack::VerifiedStackPair;
use ring::signature::KeyPair;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_stack_claim_fences_power_and_replays_after_expiry() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    settle_agent(&db).await;
    let mut capabilities = catalogue();
    capabilities.backend = "cube-kvm-v1".into();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities,
                error: None,
                terminal_sessions_used: None,
            },
        )
        .await
        .unwrap();

    let admin = system_admin();
    let current = db
        .store
        .create_cvm(&admin, create_request("stack-cvm-create-0001"))
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&current))
        .await
        .unwrap();
    let pair = StackPair {
        contract: "brocade-cvm-stack-cold-pair-v1".into(),
        architecture: "x86_64".into(),
        storage_backend: StackStorage::Plain,
        source_sha256: Some("a".repeat(64)),
        target_sha256: "b".repeat(64),
        target_bytes: 13,
        source_cubelet_sha256: Some("c".repeat(64)),
        source_shim_sha256: Some("d".repeat(64)),
        target_cubelet_sha256: "e".repeat(64),
        target_shim_sha256: "f".repeat(64),
    };
    let verified = || sign(&pair);
    let source = pair.source();
    db.store
        .poll_cvm_stack_release(
            "n1",
            StackPoll {
                installed: source.clone(),
                installable: None,
                report: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .create_cvm_stack_release(&admin, "n1", verified())
            .await,
        Err(StoreError::Conflict(_))
    ));
    let stopped = db
        .store
        .command_cvm(
            &admin,
            &current.desired.id,
            CvmCommand {
                request_id: "stack-existing-stop-01".into(),
                expected_generation: current.desired.generation,
                change: CvmChange::Stop,
            },
        )
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    let mut receipt = evidence(&stopped);
    receipt.runtime_id = None;
    receipt.network_absent = true;
    receipt.interfaces.clear();
    receipt.volume_retained = true;
    db.store.cvm_record_report("n1", receipt).await.unwrap();

    let scoped = AdminContext::new(
        "other-tenant",
        AdminRole::SystemAdmin,
        Some("unrelated".into()),
    );
    assert!(matches!(
        db.store
            .create_cvm_stack_release(&scoped, "n1", verified())
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let overview = db.store.cvm_stack_overview(&scoped).await.unwrap();
    assert!(overview.0.is_empty() && overview.1.is_empty());
    let release = db
        .store
        .create_cvm_stack_release(&admin, "n1", verified())
        .await
        .unwrap();
    let deadline_from_creation: i64 = sqlx::query_scalar(
        "SELECT start_deadline_at-floor(extract(epoch FROM created_at))::bigint
         FROM cvm_stack_releases WHERE id=$1",
    )
    .bind(release.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(deadline_from_creation, 5 * 3600);
    let roots = "1".repeat(64);
    let report = |status, roots: &str| StackReport {
        id: release.id,
        status,
        roots_sha256: Some(roots.into()),
        error: Some("untrusted agent payload".into()),
    };
    let poll = |installed, report| {
        db.store.poll_cvm_stack_release(
            "n1",
            StackPoll {
                installed,
                installable: None,
                report,
            },
        )
    };
    assert_eq!(
        db.store
            .cvm_stack_bundle_bytes("n1", &pair.target_sha256)
            .await
            .unwrap(),
        13
    );
    assert!(db
        .store
        .cvm_stack_bundle_bytes("n1", pair.source_sha256.as_deref().unwrap())
        .await
        .is_err());
    assert!(db
        .store
        .cvm_stack_bundle_bytes("n2", &pair.target_sha256)
        .await
        .is_err());
    let offer = poll(
        Some(pair.target()),
        Some(report(StackReleaseStatus::Ready, &roots)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(offer.status, StackReleaseStatus::Preparing);
    let offer = poll(
        source.clone(),
        Some(report(StackReleaseStatus::Ready, &roots)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(offer.status, StackReleaseStatus::Ready);
    assert!(db
        .store
        .cvm_stack_bundle_bytes("n1", &pair.target_sha256)
        .await
        .is_err());

    let claim = StackClaim {
        roots_sha256: roots.clone(),
    };
    sqlx::query("UPDATE node_agent_state SET xray_upgrade='{}' WHERE node_id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .claim_cvm_stack_release("n1", release.id, claim.clone())
            .await,
        Err(StoreError::Conflict(_))
    ));
    sqlx::query("UPDATE node_agent_state SET xray_upgrade=NULL WHERE node_id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let start = CvmCommand {
        request_id: "stack-existing-start-01".into(),
        expected_generation: stopped.desired.generation,
        change: CvmChange::Start,
    };
    assert!(matches!(
        db.store
            .command_cvm(&admin, &current.desired.id, start.clone())
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(db
        .store
        .claim_cvm_stack_release("n2", release.id, claim.clone())
        .await
        .is_err());
    let applying = db
        .store
        .claim_cvm_stack_release("n1", release.id, claim.clone())
        .await
        .unwrap();
    assert_eq!(applying.status, StackReleaseStatus::Applying);
    sqlx::query("UPDATE cvm_stack_releases SET start_deadline_at=1 WHERE id=$1")
        .bind(release.id)
        .execute(db.pool())
        .await
        .unwrap();
    let replay = db
        .store
        .claim_cvm_stack_release("n1", release.id, claim)
        .await
        .unwrap();
    assert_eq!(applying.artifact, replay.artifact);
    assert_eq!(applying.roots_sha256, replay.roots_sha256);
    assert_eq!(replay.status, StackReleaseStatus::Applying);
    assert!(db
        .store
        .cancel_cvm_stack_release(&admin, release.id)
        .await
        .is_err());

    for (observed, mismatch) in [
        (source, roots.as_str()),
        (Some(pair.target()), &"2".repeat(64)),
    ] {
        let offer = poll(
            observed,
            Some(report(StackReleaseStatus::Succeeded, mismatch)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(offer.status, StackReleaseStatus::Applying);
    }
    assert_eq!(
        poll(None, Some(report(StackReleaseStatus::Blocked, &roots)))
            .await
            .unwrap()
            .unwrap()
            .status,
        StackReleaseStatus::Blocked
    );
    assert!(matches!(
        db.store
            .command_cvm(&admin, &current.desired.id, start.clone())
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        poll(
            Some(pair.target()),
            Some(report(StackReleaseStatus::Recovered, &roots))
        )
        .await
        .unwrap()
        .unwrap()
        .status,
        StackReleaseStatus::Blocked
    );
    let (_, history) = db.store.cvm_stack_overview(&admin).await.unwrap();
    assert_eq!(history[0].status, StackReleaseStatus::Blocked);
    for _ in 0..2 {
        assert!(poll(
            pair.source(),
            Some(report(StackReleaseStatus::Recovered, &roots))
        )
        .await
        .unwrap()
        .is_none());
    }
    let (_, history) = db.store.cvm_stack_overview(&admin).await.unwrap();
    assert_eq!(history[0].status, StackReleaseStatus::Recovered);
    assert_eq!(
        history[0].error.as_deref(),
        Some("Cube stack switch failed; the source stack was restored")
    );
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM cvm_stack_release_events WHERE release_id=$1 ORDER BY id",
    )
    .bind(release.id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        audit,
        ["preparing", "ready", "applying", "blocked", "recovered"]
    );
    let expiring = db
        .store
        .create_cvm_stack_release(&admin, "n1", verified())
        .await
        .unwrap();
    sqlx::query("UPDATE cvm_stack_releases SET start_deadline_at=1 WHERE id=$1")
        .bind(expiring.id)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(poll(pair.source(), None).await.unwrap().is_none());
    let expired: (String, String) = sqlx::query_as(
        "SELECT status,actor_kind FROM cvm_stack_release_events WHERE release_id=$1 ORDER BY id DESC LIMIT 1",
    ).bind(expiring.id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(expired, ("cancelled".into(), "system".into()));
    assert!(db
        .store
        .command_cvm(&admin, &current.desired.id, start)
        .await
        .is_ok());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_stack_first_install_requires_fresh_evidence_and_recovers_only_to_target() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    settle_agent(&db).await;

    let admin = system_admin();
    let pair = install_pair();
    let storage = Some(pair.storage_backend);
    let roots = pair.storage_backend.empty_roots_sha256();
    let wrong_roots = StackStorage::Xfs.empty_roots_sha256();
    let create = || db.store.create_cvm_stack_release(&admin, "n1", sign(&pair));
    let poll = |installed, installable, report| {
        db.store.poll_cvm_stack_release(
            "n1",
            StackPoll {
                installed,
                installable,
                report,
            },
        )
    };
    assert!(matches!(create().await, Err(StoreError::Conflict(_))));
    assert!(matches!(
        poll(Some(pair.target()), storage, None).await,
        Err(StoreError::InvalidData(_))
    ));
    poll(None, Some(StackStorage::Xfs), None).await.unwrap();
    assert!(matches!(create().await, Err(StoreError::Conflict(_))));
    poll(None, storage, None).await.unwrap();
    let (hosts, _) = db.store.cvm_stack_overview(&admin).await.unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(
        serde_json::to_value(&hosts[0]).unwrap(),
        json!({"node_id":"n1", "installable":"plain", "observed_at":hosts[0].observed_at})
    );
    sqlx::query("UPDATE cvm_stack_observations SET observed_at=now()-interval '10 minutes'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(create().await, Err(StoreError::Conflict(_))));
    poll(None, storage, None).await.unwrap();
    poll(None, None, None).await.unwrap();
    assert!(db
        .store
        .cvm_stack_overview(&admin)
        .await
        .unwrap()
        .0
        .is_empty());
    assert!(matches!(create().await, Err(StoreError::Conflict(_))));
    poll(None, storage, None).await.unwrap();
    // Evidence from before an Agent credential rotation does not establish this host's state.
    settle_agent(&db).await;
    assert!(matches!(create().await, Err(StoreError::Conflict(_))));
    poll(None, storage, None).await.unwrap();

    let release = create().await.unwrap();
    assert_eq!(release.source_sha256, None);
    let receipt = |status, digest: &str| StackReport {
        id: release.id,
        status,
        roots_sha256: Some(digest.into()),
        error: Some("untrusted agent payload".into()),
    };
    for (evidence, digest) in [(None, roots.as_str()), (storage, wrong_roots.as_str())] {
        let offer = poll(
            None,
            evidence,
            Some(receipt(StackReleaseStatus::Ready, digest)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(offer.status, StackReleaseStatus::Preparing);
    }
    let offer = poll(
        None,
        storage,
        Some(receipt(StackReleaseStatus::Ready, &roots)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(offer.status, StackReleaseStatus::Ready);
    let claim = || {
        db.store.claim_cvm_stack_release(
            "n1",
            release.id,
            StackClaim {
                roots_sha256: roots.clone(),
            },
        )
    };
    poll(None, None, None).await.unwrap();
    assert!(matches!(claim().await, Err(StoreError::Conflict(_))));
    poll(None, storage, None).await.unwrap();
    assert_eq!(claim().await.unwrap().status, StackReleaseStatus::Applying);

    for status in [StackReleaseStatus::Recovered, StackReleaseStatus::Failed] {
        let offer = poll(None, None, Some(receipt(status, &roots)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(offer.status, StackReleaseStatus::Applying);
    }
    assert_eq!(
        poll(
            None,
            None,
            Some(receipt(StackReleaseStatus::Blocked, &roots))
        )
        .await
        .unwrap()
        .unwrap()
        .status,
        StackReleaseStatus::Blocked
    );
    let mut wrong_target = pair.target();
    wrong_target.shim_sha256 = "a".repeat(64);
    for (installed, evidence, status, digest) in [
        (None, storage, StackReleaseStatus::Recovered, roots.as_str()),
        (None, storage, StackReleaseStatus::Failed, roots.as_str()),
        (
            Some(wrong_target),
            None,
            StackReleaseStatus::Succeeded,
            roots.as_str(),
        ),
        (
            Some(pair.target()),
            None,
            StackReleaseStatus::Succeeded,
            wrong_roots.as_str(),
        ),
    ] {
        let offer = poll(installed, evidence, Some(receipt(status, digest)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(offer.status, StackReleaseStatus::Blocked);
        assert_eq!(
            db.store.cvm_stack_overview(&admin).await.unwrap().1[0].status,
            StackReleaseStatus::Blocked
        );
    }
    for _ in 0..2 {
        assert!(poll(
            Some(pair.target()),
            None,
            Some(receipt(StackReleaseStatus::Succeeded, &roots))
        )
        .await
        .unwrap()
        .is_none());
    }
    let (hosts, history) = db.store.cvm_stack_overview(&admin).await.unwrap();
    assert_eq!(history[0].status, StackReleaseStatus::Succeeded);
    assert_eq!(history[0].roots_sha256.as_deref(), Some(roots.as_str()));
    assert!(history[0].error.is_none());
    let host = serde_json::to_value(&hosts[0]).unwrap();
    assert_eq!(host["bundle_sha256"], pair.target_sha256);
    assert_eq!(host["shim_sha256"], pair.target_shim_sha256);
    assert!(host.get("installable").is_none() && host.get("installed").is_none());
    assert!(matches!(create().await, Err(StoreError::Conflict(_))));
    let succeeded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cvm_stack_release_events WHERE release_id=$1 AND status='succeeded'",
    )
    .bind(release.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(succeeded, 1);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_stack_first_install_rechecks_baremetal_assignments() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    settle_agent(&db).await;
    let admin = system_admin();
    let pair = install_pair();
    let poll = |report| {
        db.store.poll_cvm_stack_release(
            "n1",
            StackPoll {
                installed: None,
                installable: Some(pair.storage_backend),
                report,
            },
        )
    };
    let create = || db.store.create_cvm_stack_release(&admin, "n1", sign(&pair));
    poll(None).await.unwrap();
    let release = create().await.unwrap();
    let roots = pair.storage_backend.empty_roots_sha256();
    let capabilities = baremetal_tests::native_catalogue();
    let identity = capabilities
        .host_system
        .as_ref()
        .unwrap()
        .identity
        .clone()
        .unwrap();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities,
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    let instance = db
        .store
        .adopt_cvm_host(
            &admin,
            CvmAdopt {
                request_id: "stack-baremetal-adopt-01".into(),
                tenant_id: "platform.acme".into(),
                user_id: Some("alice".into()),
                node_id: "n1".into(),
                name: "Existing OS".into(),
                identity,
            },
        )
        .await
        .unwrap();
    assert!(!instance.desired.spec.execution.is_cube());
    assert!(poll(Some(StackReport {
        id: release.id,
        status: StackReleaseStatus::Ready,
        roots_sha256: Some(roots),
        error: None,
    }))
    .await
    .unwrap()
    .is_none());
    let history = db.store.cvm_stack_history(&admin).await.unwrap();
    assert_eq!(history[0].status, StackReleaseStatus::Failed);
    assert!(matches!(create().await, Err(StoreError::Conflict(_))));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_stack_cancel_and_claim_are_exclusive_and_only_proven_unstarted_work_expires() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    settle_agent(&db).await;
    let admin = system_admin();
    let pair = install_pair();
    let roots = pair.storage_backend.empty_roots_sha256();
    let poll = |report| {
        db.store.poll_cvm_stack_release(
            "n1",
            StackPoll {
                installed: None,
                installable: Some(pair.storage_backend),
                report,
            },
        )
    };
    let create = || db.store.create_cvm_stack_release(&admin, "n1", sign(&pair));
    let claim = |id| {
        db.store.claim_cvm_stack_release(
            "n1",
            id,
            StackClaim {
                roots_sha256: roots.clone(),
            },
        )
    };
    let receipt = |id, status| {
        Some(StackReport {
            id,
            status,
            roots_sha256: Some(roots.clone()),
            error: None,
        })
    };
    poll(None).await.unwrap();
    let race = create().await.unwrap();
    assert_eq!(
        poll(receipt(race.id, StackReleaseStatus::Ready))
            .await
            .unwrap()
            .unwrap()
            .status,
        StackReleaseStatus::Ready
    );
    let (cancelled, claimed) = tokio::join!(
        db.store.cancel_cvm_stack_release(&admin, race.id),
        claim(race.id)
    );
    match (cancelled, claimed) {
        (Ok(record), Err(StoreError::Conflict(_))) => {
            assert_eq!(record.status, StackReleaseStatus::Cancelled)
        }
        (Err(StoreError::Conflict(_)), Ok(offer)) => {
            assert_eq!(offer.status, StackReleaseStatus::Applying);
            assert_eq!(
                poll(receipt(race.id, StackReleaseStatus::Cancelled))
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                StackReleaseStatus::Applying
            );
            sqlx::query("UPDATE cvm_stack_releases SET start_deadline_at=floor(extract(epoch FROM clock_timestamp()))::bigint WHERE id=$1")
                .bind(race.id).execute(db.pool()).await.unwrap();
            assert_eq!(
                poll(None).await.unwrap().unwrap().status,
                StackReleaseStatus::Applying
            );
            assert!(poll(receipt(race.id, StackReleaseStatus::Cancelled))
                .await
                .unwrap()
                .is_none());
        }
        outcome => panic!("expected exactly one successful lease mutation: {outcome:?}"),
    }
    assert_eq!(
        db.store.cvm_stack_history(&admin).await.unwrap()[0].status,
        StackReleaseStatus::Cancelled
    );
    let expiring = create().await.unwrap();
    assert_eq!(
        poll(receipt(expiring.id, StackReleaseStatus::Ready))
            .await
            .unwrap()
            .unwrap()
            .status,
        StackReleaseStatus::Ready
    );
    sqlx::query("UPDATE cvm_stack_releases SET start_deadline_at=floor(extract(epoch FROM clock_timestamp()))::bigint WHERE id=$1")
        .bind(expiring.id).execute(db.pool()).await.unwrap();
    assert!(matches!(
        claim(expiring.id).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(poll(receipt(expiring.id, StackReleaseStatus::Succeeded))
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        db.store.cvm_stack_history(&admin).await.unwrap()[0].id,
        expiring.id
    );
    let scoped = AdminContext::new(
        "outside",
        AdminRole::SystemAdmin,
        Some("other-tenant".into()),
    );
    assert!(db
        .store
        .cvm_stack_history(&scoped)
        .await
        .unwrap()
        .is_empty());
    assert!(create().await.is_ok());
}

async fn settle_agent(db: &TestPg) {
    db.store.issue_node_token("n1").await.unwrap();
    sqlx::query("UPDATE node_agent_state SET last_poll_at=now(),runtime_reported_at=now(),agent_protocol_version=$1 WHERE node_id='n1'")
        .bind(brocade_deployment::protocol::AGENT_PROTOCOL_VERSION as i32).execute(db.pool()).await.unwrap();
}

fn install_pair() -> StackPair {
    StackPair {
        contract: "brocade-cvm-stack-fresh-install-v1".into(),
        architecture: "x86_64".into(),
        storage_backend: StackStorage::Plain,
        source_sha256: None,
        target_sha256: "b".repeat(64),
        target_bytes: 13,
        source_cubelet_sha256: None,
        source_shim_sha256: None,
        target_cubelet_sha256: "e".repeat(64),
        target_shim_sha256: "f".repeat(64),
    }
}

fn sign(pair: &StackPair) -> VerifiedStackPair {
    let signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[12; 32]).unwrap();
    let payload = serde_json::to_string(pair).unwrap();
    let signature = brocade_core::hash::hex_lower(signer.sign(payload.as_bytes()).as_ref());
    VerifiedStackPair::verify(
        SignedStackPair { payload, signature },
        signer.public_key().as_ref().try_into().unwrap(),
    )
    .unwrap()
}
