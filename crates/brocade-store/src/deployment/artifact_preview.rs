use super::*;

/// Compare planned bytes with acknowledged artifacts, not a historical model recompiled today.
/// This read creates no deployment or blob. Newly managed machines have an empty baseline.
pub(crate) async fn plan_deployment_artifacts(
    pool: &PgPool,
    actor: &AdminContext,
    revision_id: u64,
    include_content: bool,
) -> Result<Vec<DeploymentTargetDetail>> {
    if include_content && !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "artifact source requires system administrator access".to_owned(),
        ));
    }
    let plan =
        crate::model_timing::measure("deployment_plan", plan_deployment(pool, actor, revision_id))
            .await?;
    let nodes = plan
        .targets
        .iter()
        .map(|t| t.node_id.clone())
        .collect::<Vec<_>>();
    let rows = sqlx::query(
        "SELECT n.id, to_jsonb(s) AS applied,
                EXISTS (SELECT 1 FROM deployment_target_state d
                        WHERE d.node_id=n.id AND d.dispatched_at IS NOT NULL) AS deployed_before
           FROM nodes n LEFT JOIN node_applied_state s ON s.node_id=n.id
          WHERE n.id = ANY($1)",
    )
    .bind(&nodes)
    .fetch_all(pool)
    .await?;
    let mut before_by_node = BTreeMap::new();
    for row in rows {
        let applied: Option<Value> = row.try_get("applied")?;
        let deployed_before: bool = row.try_get("deployed_before")?;
        let mut before = serde_json::Map::new();
        for kind in ConfigArtifact::ALL {
            let field = kind.field();
            let state = applied
                .as_ref()
                .and_then(|s| s.get(format!("{field}_state")));
            let sha = applied
                .as_ref()
                .and_then(|s| s.get(format!("{field}_sha256")));
            before.insert(
                field.to_owned(),
                baseline_artifact(state, sha, deployed_before),
            );
        }
        before_by_node.insert(row.try_get::<String, _>("id")?, Value::Object(before));
    }
    let mut targets = Vec::new();
    for target in plan.targets {
        let before = before_by_node.remove(&target.node_id).ok_or_else(|| {
            StoreError::InvalidData("artifact preview target disappeared".to_owned())
        })?;
        let mut desired_structure = desired_structure_json(&target)?;
        if include_content {
            for (kind, artifact) in target.desired.artifacts() {
                if let DesiredArtifact::Present { content, .. } = artifact {
                    desired_structure[kind.field()]["content"] = json!(content);
                }
            }
        }
        let status = match target.status {
            PlannedTargetStatus::Pending => "pending",
            PlannedTargetStatus::Deferred => "deferred",
            PlannedTargetStatus::Skipped => "skipped",
        };
        targets.push(DeploymentTargetDetail {
            node_id: target.node_id,
            status: status.to_owned(),
            wave: target.wave,
            disruptive: target.disruptive,
            error: None,
            desired_structure,
            observed_before: Some(before),
            observed_after: None,
            verdict: None,
            dispatched_at: None,
        });
    }
    if include_content {
        crate::model_timing::measure(
            "artifact_load_content",
            attach_known_artifact_content(pool, &mut targets),
        )
        .await?;
    }
    Ok(targets)
}

fn baseline_artifact(state: Option<&Value>, sha: Option<&Value>, deployed_before: bool) -> Value {
    match state.and_then(Value::as_str) {
        Some("present") => json!({"state":"present", "sha256":sha}),
        Some("disabled") => json!({"state":"absent"}),
        Some("dirty") => json!({"state":"dirty"}),
        // Unknown on a fresh machine means no prior managed files; on an existing
        // machine it must not erase missing or uncertain history.
        None | Some("unknown" | "unmanaged") if !deployed_before => json!({"state":"absent"}),
        _ => json!({"state":"unknown"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_lookup_includes_every_managed_file_including_port_hopping() {
        let mut value = json!({});
        let mut blobs = BTreeMap::new();
        for kind in ConfigArtifact::ALL {
            let sha = format!("sha-{}", kind.field());
            value[kind.field()] = json!({"state":"present","sha256":sha});
            blobs.insert(sha, (format!("content-{}", kind.field()), 10));
        }
        let mut shas = Vec::new();
        collect_content_shas(&value, &mut shas);
        assert_eq!(shas.len(), ConfigArtifact::ALL.len());
        attach_content_to_value(&mut value, &blobs);
        for kind in ConfigArtifact::ALL {
            assert_eq!(
                value[kind.field()]["content"],
                format!("content-{}", kind.field())
            );
        }
    }

    #[test]
    fn new_machine_has_an_empty_baseline_not_missing_history() {
        for state in [None, Some(json!("unknown")), Some(json!("unmanaged"))] {
            assert_eq!(
                baseline_artifact(state.as_ref(), None, false),
                json!({"state":"absent"})
            );
        }
    }

    #[test]
    fn acknowledged_digest_and_existing_unknown_state_are_not_recompiled_or_erased() {
        assert_eq!(
            baseline_artifact(
                Some(&json!("present")),
                Some(&json!("old-compiler-bytes")),
                false
            ),
            json!({"state":"present","sha256":"old-compiler-bytes"})
        );
        assert_eq!(
            baseline_artifact(None, None, true),
            json!({"state":"unknown"})
        );
        assert_eq!(
            baseline_artifact(Some(&json!("dirty")), None, false),
            json!({"state":"dirty"})
        );
    }
}
