//! Recoverable Guest credentials live outside immutable public specifications.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

fn context(id: &str, revision: u64) -> String {
    format!("cvm-guest-password:{id}:{revision}")
}

pub(super) async fn generate(tx: &mut Transaction<'_, Postgres>, id: &str) -> Result<u64> {
    let revision: i64 = sqlx::query_scalar(
        "SELECT coalesce(max(revision),0)+1 FROM cvm_guest_credentials WHERE instance_id=$1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    let mut entropy = [0; 24];
    getrandom::fill(&mut entropy)?;
    let password = URL_SAFE_NO_PAD.encode(entropy);
    let sealed = crate::secrets::seal(&context(id, revision as u64), &password)?;
    sqlx::query(
        "INSERT INTO cvm_guest_credentials(instance_id,revision,password_sealed) VALUES($1,$2,$3)",
    )
    .bind(id)
    .bind(revision)
    .bind(sealed)
    .execute(&mut **tx)
    .await?;
    Ok(revision as u64)
}

impl PgStore {
    async fn guest_credential(&self, id: &str, revision: u64) -> Result<GuestCredential> {
        let sealed: String = sqlx::query_scalar(
            "SELECT password_sealed FROM cvm_guest_credentials WHERE instance_id=$1 AND revision=$2",
        ).bind(id).bind(revision as i64).fetch_optional(&self.pool).await?
            .ok_or_else(|| StoreError::NotFound("容器登录密码".into()))?;
        Ok(GuestCredential {
            instance_id: id.into(),
            revision,
            password: crate::secrets::open(&context(id, revision), &sealed)?,
        })
    }

    pub async fn cvm_login_credential(
        &self,
        actor: &AdminContext,
        id: &str,
    ) -> Result<GuestCredential> {
        let instance = self.cvm_instance(actor, id).await?;
        require_owner(actor, &instance.tenant_id, instance.user(), true)?;
        let mut tx = self.pool.begin().await?;
        let serving = releases::serving_tx(&mut tx, &instance.node_id).await?;
        tx.commit().await?;
        let approved = serving
            .and_then(|s| s.instances.into_iter().find(|i| i.id == id))
            .ok_or_else(|| StoreError::Unavailable("容器登录密码等待发布".into()))?;
        if approved.spec.login_revision != instance.desired.spec.login_revision
            || !instance.report.as_ref().is_some_and(|r| {
                r.status == ApplyStatus::Applied
                    && r.power == Some(PowerState::Running)
                    && r.generation == approved.generation
                    && r.spec_sha256 == approved.spec_sha256
            })
        {
            return Err(StoreError::Unavailable(
                "容器开机并确认登录密码生效后才能查看".into(),
            ));
        }
        let revision = approved.spec.login_revision;
        self.guest_credential(id, revision).await
    }

    pub async fn cvm_node_credential(
        &self,
        node: &str,
        id: &str,
        revision: u64,
    ) -> Result<GuestCredential> {
        let mut tx = self.pool.begin().await?;
        let desired = releases::serving_tx(&mut tx, node)
            .await?
            .ok_or_else(|| StoreError::Forbidden("节点尚无已批准容器配置".into()))?;
        if let Some(instance) = desired.instances.iter().find(|i| i.id == id) {
            ensure_access_generation(&mut tx, id, instance.generation).await?;
        }
        tx.commit().await?;
        if revision == 0
            || !desired.instances.iter().any(|i| {
                i.id == id && i.spec.login_revision == revision && i.power == PowerState::Running
            })
        {
            return Err(StoreError::Forbidden(
                "容器凭据不属于当前节点的已批准配置".into(),
            ));
        }
        self.guest_credential(id, revision).await
    }
}
