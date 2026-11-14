//! Operational user routing preferences, independent of the revision/publication pipeline.
use crate::{AdminContext, AdminRole, PgStore, Result, StoreError};
use brocade_core::{
    artifacts::subscription::{self, Subscription},
    format::yaml::{self, RoutingCatalog, RulePreferences},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;

#[derive(Serialize)]
pub struct View {
    pub version: i64,
    pub rules: RulePreferences,
    pub catalog: RoutingCatalog,
    pub writable: bool,
}
#[derive(Deserialize)]
pub struct Write {
    pub version: i64,
    pub rules: RulePreferences,
}
fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidData(message.into())
}
pub fn access(actor: &AdminContext, tenant: &str, user: &str, write: bool) -> Result<()> {
    if actor.operator_id() == crate::PUBLIC_OPERATOR_ID {
        return Err(StoreError::Forbidden("规则需要登录后访问".into()));
    }
    if actor.role() == AdminRole::User {
        return if actor
            .self_user()
            .is_some_and(|u| u.tenant_id == tenant && u.user_id == user)
        {
            Ok(())
        } else {
            Err(StoreError::Forbidden("只能访问自己的订阅规则".into()))
        };
    }
    if write
        && !matches!(
            actor.role(),
            AdminRole::SystemAdmin | AdminRole::TenantAdmin | AdminRole::Editor
        )
    {
        return Err(StoreError::Forbidden("规则只读".into()));
    }
    if !actor.is_global_scope() {
        actor.require_tenant_access(tenant, "订阅规则")?;
    }
    Ok(())
}
pub(crate) async fn load(
    pool: &sqlx::PgPool,
    tenant: &str,
    user: &str,
) -> Result<(i64, RulePreferences)> {
    let row =
        sqlx::query("SELECT version,rules FROM user_clash_rules WHERE tenant_id=$1 AND user_id=$2")
            .bind(tenant)
            .bind(user)
            .fetch_optional(pool)
            .await?;
    match row {
        Some(row) => Ok((
            row.try_get("version")?,
            serde_json::from_value(row.try_get("rules")?)
                .map_err(|_| invalid("已保存的订阅规则无法读取"))?,
        )),
        None => Ok((0, RulePreferences::default())),
    }
}
async fn artifact(
    store: &PgStore,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
) -> Result<(
    Subscription,
    std::sync::Arc<brocade_core::physical::user::UserPlan>,
)> {
    access(actor, tenant, user, false)?;
    let serving = crate::serving::load_subscription_serving_projection(store.pool()).await?;
    if !serving
        .snapshot
        .users
        .iter()
        .any(|u| u.tenant == tenant && u.id == user)
    {
        return Err(StoreError::NotFound("用户尚无已生效订阅".into()));
    }
    serving.ensure_available()?;
    let output = crate::compile_cache::compile_incremental(&serving.snapshot);
    let plan = crate::compile_cache::project_user(&output, tenant, user)
        .map_err(|_| invalid("无法生成当前用户订阅"))?;
    Ok((subscription::build(&plan), plan))
}
impl PgStore {
    pub async fn clash_rules(
        &self,
        actor: &AdminContext,
        tenant: &str,
        user: &str,
    ) -> Result<View> {
        let (artifact, plan) = artifact(self, actor, tenant, user).await?;
        let (version, rules) = load(self.pool(), tenant, user).await?;
        Ok(View {
            version,
            rules,
            catalog: yaml::routing_catalog_for_plan(&artifact, &plan),
            writable: access(actor, tenant, user, true).is_ok(),
        })
    }
    pub async fn preview_clash_rules(
        &self,
        actor: &AdminContext,
        tenant: &str,
        user: &str,
        rules: &RulePreferences,
    ) -> Result<String> {
        let (artifact, plan) = artifact(self, actor, tenant, user).await?;
        rules
            .validate(&yaml::routing_catalog_for_plan(&artifact, &plan))
            .map_err(invalid)?;
        let yaml = yaml::clash_subscription_with_rules_for_plan(&artifact, &plan, rules);
        yaml.split_once("\nproxy-groups:\n")
            .map(|(_, body)| format!("proxy-groups:\n{body}"))
            .ok_or_else(|| invalid("订阅预览缺少规则组"))
    }
    pub async fn save_clash_rules(
        &self,
        actor: &AdminContext,
        tenant: &str,
        user: &str,
        write: Write,
    ) -> Result<View> {
        access(actor, tenant, user, true)?;
        let (artifact, plan) = artifact(self, actor, tenant, user).await?;
        let catalog = yaml::routing_catalog_for_plan(&artifact, &plan);
        write.rules.validate(&catalog).map_err(invalid)?;
        let mut tx = self.pool().begin().await?;
        let present: Option<String> =
            sqlx::query_scalar("SELECT id FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
                .bind(tenant)
                .bind(user)
                .fetch_optional(&mut *tx)
                .await?;
        if present.is_none() {
            return Err(StoreError::NotFound("用户不存在".into()));
        }
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM user_clash_rules WHERE tenant_id=$1 AND user_id=$2",
        )
        .bind(tenant)
        .bind(user)
        .fetch_optional(&mut *tx)
        .await?;
        if current.unwrap_or(0) != write.version {
            return Err(StoreError::Conflict(
                "规则已在其他窗口更新，请重新载入后再保存".into(),
            ));
        }
        let version = write
            .version
            .checked_add(1)
            .ok_or_else(|| invalid("规则版本超出范围"))?;
        sqlx::query("INSERT INTO user_clash_rules(tenant_id,user_id,version,rules) VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id,user_id) DO UPDATE SET version=EXCLUDED.version,rules=EXCLUDED.rules,updated_at=now()")
            .bind(tenant).bind(user).bind(version).bind(serde_json::to_value(&write.rules).map_err(|_|invalid("规则无法保存"))?).execute(&mut *tx).await?;
        sqlx::query("UPDATE subscription_connectors SET verified=false,next_check=now() WHERE tenant_id=$1 AND user_id=$2 AND sealed IS NOT NULL").bind(tenant).bind(user).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(View {
            version,
            rules: write.rules,
            catalog,
            writable: true,
        })
    }
}
