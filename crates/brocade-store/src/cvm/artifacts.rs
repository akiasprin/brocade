//! Immutable instance products linked to the global configuration revision and release.
use super::*;

pub(super) async fn freeze(
    tx: &mut Transaction<'_, Postgres>,
    desired: &InstanceDesired,
) -> Result<()> {
    if !desired.spec.execution.is_cube() {
        return Ok(());
    }
    let previous: Option<(i64,String)> = sqlx::query_as("SELECT generation,sha256 FROM cvm_spec_versions WHERE instance_id=$1 AND generation<$2 ORDER BY generation DESC LIMIT 1")
        .bind(&desired.id).bind(desired.generation as i64).fetch_optional(&mut **tx).await?;
    if let Some((generation, sha)) = previous {
        if sha == desired.spec_sha256 {
            if let Some(bundle) = load(tx, &desired.id, generation as u64).await? {
                persist(tx, desired, &bundle).await?;
                return Ok(());
            }
            if desired.power != PowerState::Running {
                return Ok(());
            }
        }
    }
    if desired.power == PowerState::Deleted {
        return Ok(());
    }
    let row=sqlx::query("SELECT h.capabilities FROM cvm_hosts h JOIN cvm_instances i ON i.node_id=h.node_id WHERE i.id=$1")
        .bind(&desired.id).fetch_one(&mut **tx).await?;
    let caps: Capabilities = serde_json::from_value(row.try_get("capabilities")?)?;
    // Old executors have no verified template. Never fabricate products for them.
    if caps.artifact_compiler.as_deref() != Some(CVM_ARTIFACT_COMPILER) {
        return Ok(());
    }
    let image = desired
        .spec
        .image
        .as_ref()
        .ok_or_else(|| invalid("Cube image missing"))?;
    let content: Option<String> =
        sqlx::query_scalar("SELECT content FROM cvm_image_manifests WHERE sha256=$1")
            .bind(&image.sha256)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(content) = content else {
        return Err(StoreError::Conflict(
            "宿主镜像清单尚未校验，请等待 Agent 上报后重试".into(),
        ));
    };
    let bundle = brocade_cvm_compile::compile(&CvmArtifactInput {
        instance_id: desired.id.clone(),
        spec: desired.spec.clone(),
        capabilities: caps,
        image_manifest: CvmImageManifest {
            sha256: image.sha256.clone(),
            content,
        },
    })
    .map_err(invalid)?;
    persist(tx, desired, &bundle).await
}
async fn persist(
    tx: &mut Transaction<'_, Postgres>,
    desired: &InstanceDesired,
    bundle: &CvmArtifactBundle,
) -> Result<()> {
    sqlx::query("INSERT INTO cvm_spec_artifact_sets(instance_id,generation,compiler,input_sha256,spec_sha256,input) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(&desired.id).bind(desired.generation as i64).bind(&bundle.compiler).bind(&bundle.input_sha256).bind(&bundle.spec_sha256).bind(serde_json::to_value(&bundle.input)?).execute(&mut **tx).await?;
    for file in &bundle.files {
        sqlx::query("INSERT INTO cvm_spec_artifacts(instance_id,generation,kind,sha256,content) VALUES($1,$2,$3,$4,$5)")
            .bind(&desired.id).bind(desired.generation as i64).bind(serde_json::to_value(file.kind)?.as_str().ok_or_else(|| invalid("artifact kind"))?).bind(&file.sha256).bind(&file.content).execute(&mut **tx).await?;
    }
    Ok(())
}
pub(super) async fn load(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    generation: u64,
) -> Result<Option<CvmArtifactBundle>> {
    let row=sqlx::query("SELECT compiler,input_sha256,spec_sha256,input FROM cvm_spec_artifact_sets WHERE instance_id=$1 AND generation=$2").bind(id).bind(generation as i64).fetch_optional(&mut **tx).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let rows=sqlx::query("SELECT kind,sha256,content FROM cvm_spec_artifacts WHERE instance_id=$1 AND generation=$2 ORDER BY kind").bind(id).bind(generation as i64).fetch_all(&mut **tx).await?;
    let mut files = Vec::new();
    for row in rows {
        files.push(CvmArtifact {
            kind: serde_json::from_value(serde_json::Value::String(row.try_get("kind")?))?,
            sha256: row.try_get("sha256")?,
            content: row.try_get("content")?,
        });
    }
    files.sort_by_key(|f| f.kind);
    Ok(Some(CvmArtifactBundle {
        compiler: row.try_get("compiler")?,
        input_sha256: row.try_get("input_sha256")?,
        spec_sha256: row.try_get("spec_sha256")?,
        input: serde_json::from_value(row.try_get("input")?)?,
        files,
    }))
}
#[derive(Debug, Serialize)]
pub struct CvmArtifactHistory {
    pub generation: u64,
    pub current: Option<CvmArtifactBundle>,
    pub previous: Option<CvmArtifactBundle>,
}
#[derive(Default)]
pub struct CvmMutationReview {
    pub preview: bool,
    pub expected_input_sha256: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct CvmMutationResult {
    pub instance: CvmInstance,
    pub artifacts: Option<CvmArtifactHistory>,
    pub no_op: bool,
}
impl CvmMutationResult {
    pub(super) fn existing(instance: CvmInstance) -> Self {
        Self {
            instance,
            artifacts: None,
            no_op: true,
        }
    }
}
pub(super) async fn check_review(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    review: &CvmMutationReview,
) -> Result<()> {
    if let Some(expected) = review.expected_input_sha256.as_deref() {
        let generation: i64 =
            sqlx::query_scalar("SELECT generation FROM cvm_instances WHERE id=$1")
                .bind(id)
                .fetch_one(&mut **tx)
                .await?;
        let bundle = load(tx, id, generation as u64).await?;
        if bundle.as_ref().is_none_or(|c| c.input_sha256 != expected) {
            return Err(StoreError::Conflict(
                "编译输入已变化，请重新预览后提交；未保存任何改动".into(),
            ));
        }
    }
    Ok(())
}
impl PgStore {
    pub(super) async fn finish_cvm_review(
        &self,
        mut tx: Transaction<'_, Postgres>,
        actor: &AdminContext,
        id: &str,
        review: CvmMutationReview,
        no_op: bool,
    ) -> Result<CvmMutationResult> {
        let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1"))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let instance = from_row(&row)?;
        let generation = instance.desired.generation;
        let current = load(&mut tx, id, generation).await?;
        if let Some(expected) = review.expected_input_sha256.as_deref() {
            if current.as_ref().is_none_or(|c| c.input_sha256 != expected) {
                return Err(StoreError::Conflict(
                    "编译输入已变化，请重新预览后提交；未保存任何改动".into(),
                ));
            }
        }
        if review.preview {
            let serving = releases::serving_tx(&mut tx, &instance.node_id).await?;
            let previous = match serving.and_then(|s| s.instances.into_iter().find(|i| i.id == id))
            {
                Some(i) => load(&mut tx, id, i.generation).await?,
                None => None,
            };
            tx.rollback().await?;
            Ok(CvmMutationResult {
                instance,
                artifacts: Some(CvmArtifactHistory {
                    generation,
                    current,
                    previous,
                }),
                no_op,
            })
        } else {
            tx.commit().await?;
            Ok(CvmMutationResult::existing(
                self.cvm_instance(actor, id).await?,
            ))
        }
    }
}
impl PgStore {
    pub async fn cvm_missing_image_manifests(&self, node: &str) -> Result<Vec<String>> {
        let caps: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT capabilities FROM cvm_hosts WHERE node_id=$1")
                .bind(node)
                .fetch_optional(&self.pool)
                .await?;
        let Some(caps) = caps else {
            return Ok(Vec::new());
        };
        let caps: Capabilities = serde_json::from_value(caps)?;
        let mut missing = Vec::new();
        if caps.artifact_compiler.as_deref() == Some(CVM_ARTIFACT_COMPILER) {
            for image in caps.images {
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM cvm_image_manifests WHERE sha256=$1)",
                )
                .bind(&image.sha256)
                .fetch_one(&self.pool)
                .await?;
                if !exists && !missing.contains(&image.sha256) {
                    missing.push(image.sha256);
                }
            }
        }
        Ok(missing)
    }
    pub async fn cvm_record_image_manifest(
        &self,
        node: &str,
        manifest: CvmImageManifest,
    ) -> Result<()> {
        brocade_cvm_compile::normalize_image(&manifest).map_err(invalid)?;
        let mut tx = self.pool.begin().await?;
        let caps: serde_json::Value =
            sqlx::query_scalar("SELECT capabilities FROM cvm_hosts WHERE node_id=$1 FOR SHARE")
                .bind(node)
                .fetch_one(&mut *tx)
                .await?;
        let caps: Capabilities = serde_json::from_value(caps)?;
        if caps.artifact_compiler.as_deref() != Some(CVM_ARTIFACT_COMPILER)
            || !caps.images.iter().any(|i| i.sha256 == manifest.sha256)
        {
            return Err(StoreError::Forbidden(
                "镜像清单不属于节点已上报的校验目录".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO cvm_image_manifests(sha256,content) VALUES($1,$2) ON CONFLICT DO NOTHING",
        )
        .bind(&manifest.sha256)
        .bind(&manifest.content)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn cvm_artifacts(
        &self,
        actor: &AdminContext,
        id: &str,
        generation: Option<u64>,
    ) -> Result<CvmArtifactHistory> {
        // Authorize the instance before touching history. Readonly does not expose raw templates.
        let instance = self.cvm_instance(actor, id).await?;
        if actor.role() == AdminRole::Readonly {
            return Err(StoreError::Forbidden("只读角色不能读取实例产物内容".into()));
        }
        let generation = generation.unwrap_or(instance.desired.generation);
        if generation == 0
            || generation > instance.desired.generation
            || generation > i64::MAX as u64
        {
            return Err(invalid("实例代号无效"));
        }
        let mut tx = self.pool.begin().await?;
        let current = load(&mut tx, id, generation).await?;
        let previous_generation: Option<i64> = sqlx::query_scalar(
            "SELECT max(generation) FROM cvm_spec_versions WHERE instance_id=$1 AND generation<$2",
        )
        .bind(id)
        .bind(generation as i64)
        .fetch_one(&mut *tx)
        .await?;
        let previous = match previous_generation {
            Some(g) => load(&mut tx, id, g as u64).await?,
            None => None,
        };
        tx.commit().await?;
        Ok(CvmArtifactHistory {
            generation,
            current,
            previous,
        })
    }
}
