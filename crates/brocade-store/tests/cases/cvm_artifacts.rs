use super::*;
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_artifacts_freeze_atomically_verify_inputs_and_scope_history() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let input: CvmArtifactInput = serde_json::from_str(include_str!(
        "../../../brocade-cvm-compile/tests/golden/input.json"
    ))
    .unwrap();
    let mut caps = catalogue();
    caps.artifact_compiler = Some(CVM_ARTIFACT_COMPILER.into());
    caps.cube_storage_backend = Some("plain".into());
    caps.images[0].sha256 = input.image_manifest.sha256.clone();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps.clone(),
                error: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store.cvm_missing_image_manifests("n1").await.unwrap(),
        vec![input.image_manifest.sha256.clone()]
    );
    assert!(({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .create_cvm(
        &actor_user("alice"),
        create_request("artifact-before-upload-01")
    )
    .await
    .is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_instances")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let mut bad = input.image_manifest.clone();
    bad.content.push(' ');
    assert!(db.store.cvm_record_image_manifest("n1", bad).await.is_err());
    db.store
        .cvm_record_image_manifest("n1", input.image_manifest.clone())
        .await
        .unwrap();
    db.store
        .cvm_record_image_manifest("n1", input.image_manifest.clone())
        .await
        .unwrap();
    assert!(db
        .store
        .cvm_missing_image_manifests("n1")
        .await
        .unwrap()
        .is_empty());
    let item = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .create_cvm(
        &actor_user("alice"),
        create_request("artifact-create-request-01"),
    )
    .await
    .unwrap();
    let h = db
        .store
        .cvm_artifacts(&actor_user("alice"), &item.desired.id, None)
        .await
        .unwrap();
    let bundle = h.current.unwrap();
    assert_eq!(bundle.files.len(), 4);
    assert!(h.previous.is_none());
    let sent = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_desired_page("n1", Some(0))
    .await
    .unwrap();
    assert_eq!(sent.instances[0].artifacts.as_ref(), Some(&bundle));
    brocade_cvm_compile::verify(&bundle, &sent.instances[0]).unwrap();
    assert!(db
        .store
        .cvm_artifacts(&actor_user("bob"), &item.desired.id, None)
        .await
        .is_err());
    for query in [
        "UPDATE cvm_spec_artifacts SET content='{}'",
        "DELETE FROM cvm_spec_artifact_sets",
        "UPDATE cvm_image_manifests SET content='{}'",
    ] {
        assert!(sqlx::query(query).execute(db.pool()).await.is_err());
    }
    // New inventory does not mutate/recompile a previously frozen generation.
    caps.allocatable_disk_bytes += 1024;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps,
                error: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store
            .cvm_artifacts(&actor_user("alice"), &item.desired.id, None)
            .await
            .unwrap()
            .current,
        Some(bundle.clone())
    );
    // A receipt must commit to this exact stored input and file set.
    let mut report = evidence(&item);
    report.artifacts = Some(CvmArtifactReceipt {
        compiler: bundle.compiler.clone(),
        input_sha256: "f".repeat(64),
        mode: CvmArtifactMode::Shadow,
        mismatches: Vec::new(),
        files: Vec::new(),
    });
    assert!(({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", report)
    .await
    .is_err());
    ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", evidence(&item))
    .await
    .unwrap();
    let next = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .command_cvm(
        &actor_user("alice"),
        &item.desired.id,
        CvmCommand {
            request_id: "artifact-restart-request-01".into(),
            expected_generation: 1,
            change: CvmChange::Restart,
        },
    )
    .await
    .unwrap();
    let h = db
        .store
        .cvm_artifacts(&actor_user("alice"), &item.desired.id, None)
        .await
        .unwrap();
    assert_eq!(h.generation, next.desired.generation);
    assert_eq!(h.previous, Some(bundle.clone()));
    assert_eq!(h.current.unwrap().files, bundle.files);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_artifacts_pagination_and_cleanup_preserve_legacy_and_retention() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let input: CvmArtifactInput = serde_json::from_str(include_str!(
        "../../../brocade-cvm-compile/tests/golden/input.json"
    ))
    .unwrap();
    let mut caps = catalogue();
    caps.artifact_compiler = Some(CVM_ARTIFACT_COMPILER.into());
    caps.cube_storage_backend = Some("plain".into());
    caps.images[0].sha256 = input.image_manifest.sha256.clone();
    caps.allocatable_vcpus = 32;
    caps.allocatable_disk_bytes = 64 * 1024 * 1024 * 1024;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps.clone(),
                error: None,
            },
        )
        .await
        .unwrap();
    db.store
        .cvm_record_image_manifest("n1", input.image_manifest)
        .await
        .unwrap();
    let mut q = quota();
    q.max_instances = 12;
    q.max_vcpus = 32;
    q.max_memory_bytes = 4 * 1024 * 1024 * 1024;
    q.max_disk_bytes = 48 * 1024 * 1024 * 1024;
    q.max_interfaces = 12;
    db.store.set_cvm_quota(&system_admin(), q).await.unwrap();
    let mut created = Vec::new();
    for index in 0..9 {
        created.push(
            ({
                supersede_cvm_fixture(&db).await;
                &db.store
            })
            .create_cvm(
                &actor_user("alice"),
                create_request(&format!("artifact-page-create-{index:04}")),
            )
            .await
            .unwrap(),
        );
    }
    let first = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_desired_page("n1", Some(0))
    .await
    .unwrap();
    assert_eq!(first.instances.len(), 8);
    assert_eq!(first.next_offset, Some(8));
    assert!(first.instances.iter().all(|d| d.artifacts.is_some()));
    let last = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_desired_page("n1", Some(8))
    .await
    .unwrap();
    assert_eq!(last.instances.len(), 1);
    assert_eq!(last.next_offset, None);
    assert!(first.instances.iter().all(|d| d.id != last.instances[0].id));
    let legacy = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_desired("n1")
    .await
    .unwrap();
    assert_eq!(legacy.instances.len(), 9);
    assert!(legacy.page_offset.is_none() && legacy.instances.iter().all(|d| d.artifacts.is_none()));
    let item = &created[0];
    let b = db
        .store
        .cvm_artifacts(&actor_user("alice"), &item.desired.id, None)
        .await
        .unwrap()
        .current
        .unwrap();
    let mut r = evidence(item);
    r.artifacts = Some(CvmArtifactReceipt {
        compiler: b.compiler.clone(),
        input_sha256: b.input_sha256.clone(),
        mode: CvmArtifactMode::Shadow,
        mismatches: Vec::new(),
        files: b
            .files
            .iter()
            .map(|f| CvmArtifactApplied {
                kind: f.kind,
                compiled_sha256: f.sha256.clone(),
                effective_sha256: None,
                bindings: json!({"runtime_id":"private-runtime"}),
            })
            .collect(),
    });
    ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", r)
    .await
    .unwrap();
    let viewer = AdminContext::new("viewer", AdminRole::Readonly, None);
    assert!(db
        .store
        .cvm_artifacts(&viewer, &item.desired.id, None)
        .await
        .is_err());
    assert!(db
        .store
        .cvm_instance(&viewer, &item.desired.id)
        .await
        .unwrap()
        .report
        .unwrap()
        .artifacts
        .is_none());
    caps.images[0].sha256 = "c".repeat(64);
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps,
                error: None,
            },
        )
        .await
        .unwrap();
    let stopped = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .command_cvm(
        &actor_user("alice"),
        &item.desired.id,
        CvmCommand {
            request_id: "artifact-cleanup-stop-01".into(),
            expected_generation: 1,
            change: CvmChange::Stop,
        },
    )
    .await
    .unwrap();
    assert_eq!(stopped.desired.power, PowerState::Stopped);
    assert_eq!(
        db.store
            .cvm_artifacts(&actor_user("alice"), &item.desired.id, None)
            .await
            .unwrap()
            .current,
        Some(b)
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_compile_preview_rolls_back_and_reviewed_save_commits_identical_bytes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let input: CvmArtifactInput = serde_json::from_str(include_str!(
        "../../../brocade-cvm-compile/tests/golden/input.json"
    ))
    .unwrap();
    let mut caps = catalogue();
    caps.artifact_compiler = Some(CVM_ARTIFACT_COMPILER.into());
    caps.cube_storage_backend = Some("plain".into());
    caps.images[0].sha256 = input.image_manifest.sha256.clone();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps,
                error: None,
            },
        )
        .await
        .unwrap();
    db.store
        .cvm_record_image_manifest("n1", input.image_manifest)
        .await
        .unwrap();
    let actor = actor_user("alice");
    let item = db
        .store
        .create_cvm(&actor, create_request("compile-preview-create-0001"))
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    supersede_cvm_fixture(&db).await;
    let revision =
        sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let sequence = sqlx::query_scalar::<_, i64>("SELECT last_value FROM revisions_id_seq")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cvm_operations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut routing = item.desired.spec.routing.clone();
    routing.dns = vec!["1.1.1.1".parse().unwrap()];
    let request = RoutesWrite {
        request_id: "compile-preview-routes-0001".into(),
        expected_generation: item.desired.generation,
        expected_revision: routing.revision,
        routes: routing,
    };
    let preview = db
        .store
        .set_cvm_routes_reviewed(
            &actor,
            &item.desired.id,
            request.clone(),
            CvmMutationReview {
                preview: true,
                expected_input_sha256: None,
            },
        )
        .await
        .unwrap();
    let compiled = preview.artifacts.unwrap().current.unwrap();
    assert!(!preview.no_op);
    assert_eq!(
        db.store
            .cvm_instance(&actor, &item.desired.id)
            .await
            .unwrap()
            .desired,
        item.desired
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        revision
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM revisions_id_seq")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        sequence,
        "previews do not consume a global revision number"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cvm_operations")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        count
    );
    assert!(db
        .store
        .set_cvm_routes_reviewed(
            &actor_user("bob"),
            &item.desired.id,
            request.clone(),
            CvmMutationReview {
                preview: true,
                expected_input_sha256: None
            }
        )
        .await
        .is_err());
    assert!(db
        .store
        .set_cvm_routes_reviewed(
            &actor,
            &item.desired.id,
            request.clone(),
            CvmMutationReview {
                preview: false,
                expected_input_sha256: Some("f".repeat(64))
            }
        )
        .await
        .is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM revisions_id_seq")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        sequence,
        "a stale review fails before creating a revision"
    );
    let saved = db
        .store
        .set_cvm_routes_reviewed(
            &actor,
            &item.desired.id,
            request,
            CvmMutationReview {
                preview: false,
                expected_input_sha256: Some(compiled.input_sha256.clone()),
            },
        )
        .await
        .unwrap()
        .instance;
    let frozen = db
        .store
        .cvm_artifacts(&actor, &item.desired.id, None)
        .await
        .unwrap()
        .current
        .unwrap();
    assert_eq!(
        frozen, compiled,
        "the preview and saved native products must match byte for byte"
    );
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&saved))
        .await
        .unwrap();
    supersede_cvm_fixture(&db).await;
    let current_revision =
        sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let noop = db
        .store
        .set_cvm_routes(
            &actor,
            &item.desired.id,
            RoutesWrite {
                request_id: "compile-preview-noop-0001".into(),
                expected_generation: saved.desired.generation,
                expected_revision: saved.desired.spec.routing.revision,
                routes: saved.desired.spec.routing.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(noop.desired.generation, saved.desired.generation);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        current_revision
    );
}
