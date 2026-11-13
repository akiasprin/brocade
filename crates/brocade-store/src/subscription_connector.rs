//! Per-user connector configuration and durable, bounded execution history.
//! Secret payloads are bound to both tenant and user and never returned by read APIs.
use crate::{secrets, AdminContext, AdminRole, Result, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Row};

#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub endpoint: String,
    pub login: String,
    pub password: String,
}
#[derive(Default, Deserialize)]
pub struct ConfigWrite {
    pub endpoint: Option<String>,
    pub title: String,
    #[serde(default)]
    pub auth: bool,
    #[serde(default)]
    pub login: String,
    pub password: Option<String>,
    #[serde(default)]
    pub auto: bool,
    pub version: Option<i64>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub version: i64,
    pub host: String,
    pub title: String,
    pub auth: bool,
    pub login: String,
    pub has_password: bool,
    pub auto: bool,
    pub registered: bool,
    pub verified: bool,
    pub last_confirmed: Option<String>,
    pub next_check: Option<String>,
    pub source_version: Option<i64>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub name: String,
    pub checkpoint: usize,
    pub state: String,
    pub ms: Option<u64>,
    pub http: Option<u16>,
    pub detail: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub number: usize,
    pub status: String,
    pub events: Vec<Event>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub trigger: String,
    pub actor: String,
    pub config_version: i64,
    pub source_version: Option<i64>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub parent: Option<i64>,
    pub attempts: Vec<Attempt>,
    pub error: Option<String>,
    pub output_count: Option<usize>,
}
#[derive(Serialize)]
pub struct View {
    pub writable: bool,
    pub version: i64,
    pub config: Option<Config>,
    pub inactive: bool,
    pub active: Option<Run>,
    pub latest: Option<Run>,
}
#[derive(Serialize)]
pub struct RunPage {
    pub runs: Vec<Run>,
    pub next: Option<i64>,
}

fn context(tenant: &str, user: &str) -> String {
    format!("subscription-connector:{}", json!([tenant, user]))
}
pub fn access(actor: &AdminContext, tenant: &str, user: &str, write: bool) -> Result<()> {
    if actor.operator_id() == crate::PUBLIC_OPERATOR_ID {
        return Err(StoreError::Forbidden("connector access denied".into()));
    }
    if actor.role() == AdminRole::User {
        return if actor
            .self_user()
            .is_some_and(|u| u.tenant_id == tenant && u.user_id == user)
        {
            Ok(())
        } else {
            Err(StoreError::Forbidden(
                "connector belongs to another user".into(),
            ))
        };
    }
    if write
        && !matches!(
            actor.role(),
            AdminRole::SystemAdmin | AdminRole::TenantAdmin | AdminRole::Editor
        )
    {
        return Err(StoreError::Forbidden("connector is read-only".into()));
    }
    if !actor.is_global_scope() {
        actor.require_tenant_access(tenant, "connector")?;
    }
    Ok(())
}
async fn exists(pool: &PgPool, tenant: &str, user: &str) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT status<>'active' OR service_suspended OR user_service_state(service_starts_at,service_expires_at,now())<>'active' FROM users WHERE tenant_id=$1 AND id=$2",
    )
    .bind(tenant)
    .bind(user)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound("user".into()))
}
pub async fn credentials(pool: &PgPool, tenant: &str, user: &str) -> Result<Option<Credentials>> {
    let sealed: Option<String> = sqlx::query_scalar(
        "SELECT sealed FROM subscription_connectors WHERE tenant_id=$1 AND user_id=$2",
    )
    .bind(tenant)
    .bind(user)
    .fetch_optional(pool)
    .await?
    .flatten();
    sealed
        .map(|s| {
            serde_json::from_str(&secrets::open(&context(tenant, user), &s)?)
                .map_err(|_| StoreError::InvalidData("invalid sealed connector".into()))
        })
        .transpose()
}
pub async fn view(pool: &PgPool, actor: &AdminContext, tenant: &str, user: &str) -> Result<View> {
    access(actor, tenant, user, false)?;
    let inactive = exists(pool, tenant, user).await?;
    let row=sqlx::query("SELECT version,metadata,registered,verified,(to_jsonb(last_confirmed)#>>'{}') AS last_confirmed,(to_jsonb(next_check)#>>'{}') AS next_check,source_version FROM subscription_connectors WHERE tenant_id=$1 AND user_id=$2 AND sealed IS NOT NULL")
        .bind(tenant).bind(user).fetch_optional(pool).await?;
    let config = row
        .map(|r| -> Result<Config> {
            let mut c: Config = serde_json::from_value(r.try_get("metadata")?)
                .map_err(|_| StoreError::InvalidData("invalid connector metadata".into()))?;
            c.version = r.try_get("version")?;
            c.registered = r.try_get("registered")?;
            c.verified = r.try_get("verified")?;
            c.last_confirmed = r.try_get("last_confirmed")?;
            c.next_check = if c.auto {
                r.try_get("next_check")?
            } else {
                None
            };
            c.source_version = r.try_get("source_version")?;
            Ok(c)
        })
        .transpose()?;
    let runs = list(pool, actor, tenant, user, None, false).await?.runs;
    let version: Option<i64> = sqlx::query_scalar(
        "SELECT version FROM subscription_connectors WHERE tenant_id=$1 AND user_id=$2",
    )
    .bind(tenant)
    .bind(user)
    .fetch_optional(pool)
    .await?;
    Ok(View {
        writable: access(actor, tenant, user, true).is_ok(),
        version: version.unwrap_or(0),
        config,
        inactive,
        active: runs.iter().find(|r| is_active(&r.status)).cloned(),
        latest: runs.first().cloned(),
    })
}
pub async fn save(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    input: ConfigWrite,
) -> Result<()> {
    access(actor, tenant, user, true)?;
    exists(pool, tenant, user).await?;
    if input.title.trim().is_empty()
        || input.title.len() > 128
        || input.login.len() > 256
        || input.password.as_ref().is_some_and(|p| p.len() > 4096)
    {
        return Err(StoreError::InvalidData("invalid connector fields".into()));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(jsonb_build_array($1::text,$2::text)::text,72117))").bind(tenant).bind(user).execute(&mut *tx).await?;
    let old=sqlx::query("SELECT version,sealed,remote_name,metadata FROM subscription_connectors WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE").bind(tenant).bind(user).fetch_optional(&mut *tx).await?;
    let version = old
        .as_ref()
        .map(|r| r.get::<i64, _>("version"))
        .unwrap_or(0);
    if input.version.unwrap_or(0) != version {
        return Err(StoreError::Conflict(
            "连接设置已变化，请重新打开设置".into(),
        ));
    }
    let previous: Option<Credentials> = old
        .as_ref()
        .and_then(|r| r.get::<Option<String>, _>("sealed"))
        .map(|s| {
            serde_json::from_str(&secrets::open(&context(tenant, user), &s)?)
                .map_err(|_| StoreError::InvalidData("invalid sealed connector".into()))
        })
        .transpose()?;
    let endpoint = input
        .endpoint
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().trim_end_matches('/').to_owned())
        .or_else(|| previous.as_ref().map(|c| c.endpoint.clone()))
        .ok_or_else(|| StoreError::InvalidData("填写后端地址".into()))?;
    let changed = previous.as_ref().is_none_or(|c| c.endpoint != endpoint);
    let password = if input.auth {
        input
            .password
            .filter(|s| !s.is_empty())
            .or_else(|| {
                previous
                    .as_ref()
                    .filter(|_| !changed)
                    .map(|c| c.password.clone())
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let login = if input.auth {
        if input.login.trim().is_empty() {
            previous
                .as_ref()
                .filter(|_| !changed)
                .map(|c| c.login.clone())
                .unwrap_or_default()
        } else {
            input.login.trim().to_owned()
        }
    } else {
        String::new()
    };
    if input.auth && (login.is_empty() || password.is_empty()) {
        return Err(StoreError::InvalidData("填写代理用户名和密码".into()));
    }
    // The console validates URL syntax and outbound policy before reaching this layer.
    let host = endpoint
        .split("://")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .ok_or_else(|| StoreError::InvalidData("invalid endpoint".into()))?
        .to_owned();
    let secret = Credentials {
        endpoint,
        login,
        password,
    };
    let invalidate = changed
        || old.as_ref().is_none_or(|r| {
            r.get::<serde_json::Value, _>("metadata")
                .get("title")
                .and_then(serde_json::Value::as_str)
                != Some(input.title.trim())
        })
        || previous
            .as_ref()
            .is_none_or(|p| p.login != secret.login || p.password != secret.password);
    let sealed = secrets::seal(
        &context(tenant, user),
        &serde_json::to_string(&secret)
            .map_err(|_| StoreError::InvalidData("connector encoding failed".into()))?,
    )?;
    let c = Config {
        version: version + 1,
        host,
        title: input.title.trim().into(),
        auth: input.auth,
        login: String::new(),
        has_password: input.auth,
        auto: input.auto,
        registered: false,
        verified: false,
        last_confirmed: None,
        next_check: None,
        source_version: None,
    };
    let remote = if changed {
        format!("brocade-{}", crate::credentials::generate_uuid_v4()?)
    } else {
        old.as_ref()
            .map(|r| r.get("remote_name"))
            .unwrap_or_default()
    };
    sqlx::query("INSERT INTO subscription_connectors(tenant_id,user_id,version,sealed,metadata,remote_name,auto) VALUES($1,$2,$3,$4,$5,$6,$7)
        ON CONFLICT(tenant_id,user_id) DO UPDATE SET version=$3,sealed=$4,metadata=$5,remote_name=$6,auto=$7,next_check=now(),
        registered=CASE WHEN $8 THEN false ELSE subscription_connectors.registered END,verified=CASE WHEN $9 THEN false ELSE subscription_connectors.verified END,last_confirmed=CASE WHEN $8 THEN NULL ELSE subscription_connectors.last_confirmed END")
        .bind(tenant).bind(user).bind(version+1).bind(sealed).bind(json!(c)).bind(remote).bind(input.auto).bind(changed).bind(invalidate).execute(&mut *tx).await?;
    sqlx::query("UPDATE subscription_connector_runs SET status='superseded',ended_at=now() WHERE tenant_id=$1 AND user_id=$2 AND status IN ('queued','retry_wait')").bind(tenant).bind(user).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn remove(pool: &PgPool, actor: &AdminContext, tenant: &str, user: &str) -> Result<()> {
    access(actor, tenant, user, true)?;
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(jsonb_build_array($1::text,$2::text)::text,72117))").bind(tenant).bind(user).execute(&mut *tx).await?;
    sqlx::query("UPDATE subscription_connectors SET version=version+1,sealed=NULL,auto=false,registered=false,verified=false,last_confirmed=NULL WHERE tenant_id=$1 AND user_id=$2").bind(tenant).bind(user).execute(&mut *tx).await?;
    sqlx::query("UPDATE subscription_connector_runs SET status='canceled',ended_at=now() WHERE tenant_id=$1 AND user_id=$2 AND status IN ('queued','running','retry_wait')").bind(tenant).bind(user).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
pub fn is_active(status: &str) -> bool {
    matches!(status, "queued" | "running" | "retry_wait")
}
fn run_row(r: sqlx::postgres::PgRow) -> Result<Run> {
    Ok(Run {
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        status: r.try_get("status")?,
        trigger: r.try_get("trigger")?,
        actor: r.try_get("actor")?,
        config_version: r.try_get("config_version")?,
        source_version: r.try_get("source_version")?,
        started_at: r.try_get("started_at")?,
        ended_at: r.try_get("ended_at")?,
        duration_ms: r.try_get("duration_ms")?,
        parent: r.try_get("parent")?,
        attempts: serde_json::from_value(r.try_get("attempts")?)
            .map_err(|_| StoreError::InvalidData("invalid run history".into()))?,
        error: r.try_get("error")?,
        output_count: r
            .try_get::<Option<i32>, _>("output_count")?
            .map(|v| v as usize),
    })
}
const RUN_SELECT:&str="SELECT id,kind,status,trigger,actor,config_version,source_version,(to_jsonb(started_at)#>>'{}') AS started_at,(to_jsonb(ended_at)#>>'{}') AS ended_at,CASE WHEN ended_at IS NULL THEN NULL ELSE greatest(0,(extract(epoch FROM ended_at-started_at)*1000)::bigint) END AS duration_ms,parent,attempts,error,output_count FROM subscription_connector_runs";
pub async fn list(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    before: Option<i64>,
    notable: bool,
) -> Result<RunPage> {
    access(actor, tenant, user, false)?;
    exists(pool, tenant, user).await?;
    let rows=sqlx::query(&format!("{RUN_SELECT} WHERE tenant_id=$1 AND user_id=$2 AND ($3::bigint IS NULL OR id<$3) AND (NOT $4 OR kind<>'inspect' OR status<>'succeeded') ORDER BY id DESC LIMIT 21"))
        .bind(tenant).bind(user).bind(before).bind(notable).fetch_all(pool).await?;
    let mut runs = rows.into_iter().map(run_row).collect::<Result<Vec<_>>>()?;
    let next = if runs.len() > 20 {
        runs.truncate(20);
        runs.last().map(|r| r.id)
    } else {
        None
    };
    Ok(RunPage { runs, next })
}
pub async fn run(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    id: i64,
) -> Result<Run> {
    access(actor, tenant, user, false)?;
    let row = sqlx::query(&format!(
        "{RUN_SELECT} WHERE tenant_id=$1 AND user_id=$2 AND id=$3"
    ))
    .bind(tenant)
    .bind(user)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound("connector run".into()))?;
    run_row(row)
}
pub async fn enqueue(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    kind: &str,
    parent: Option<i64>,
    trigger: &str,
) -> Result<i64> {
    access(actor, tenant, user, true)?;
    if exists(pool, tenant, user).await? && kind != "cleanup" {
        return Err(StoreError::InvalidData("用户服务已暂停".into()));
    }
    if !matches!(kind, "sync" | "verify" | "inspect" | "cleanup") {
        return Err(StoreError::InvalidData("invalid task type".into()));
    }
    if let Some(id) = parent {
        run(pool, actor, tenant, user, id).await?;
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *tx)
        .await?;
    let version:i64=sqlx::query_scalar("SELECT version FROM subscription_connectors WHERE tenant_id=$1 AND user_id=$2 AND sealed IS NOT NULL FOR UPDATE").bind(tenant).bind(user).fetch_optional(&mut *tx).await?.ok_or_else(||StoreError::NotFound("connector".into()))?;
    let existing:Option<(i64,String)>=sqlx::query_as("SELECT id,kind FROM subscription_connector_runs WHERE tenant_id=$1 AND user_id=$2 AND status IN ('queued','running','retry_wait')").bind(tenant).bind(user).fetch_optional(&mut *tx).await?;
    if let Some((id, current_kind)) = existing {
        if current_kind != kind
            && !matches!(
                (current_kind.as_str(), kind),
                ("sync", "inspect") | ("inspect", "sync")
            )
        {
            return Err(StoreError::Conflict("此用户已有其他同步任务在执行".into()));
        }
        return Ok(id);
    }
    let id=sqlx::query_scalar("INSERT INTO subscription_connector_runs(tenant_id,user_id,config_version,kind,trigger,actor,parent) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id").bind(tenant).bind(user).bind(version).bind(kind).bind(trigger).bind(actor.operator_id()).bind(parent).fetch_one(&mut *tx).await?;
    sqlx::query("UPDATE subscription_connectors SET next_check=now()+interval '5 minutes' WHERE tenant_id=$1 AND user_id=$2").bind(tenant).bind(user).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(id)
}
pub async fn cancel(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    id: i64,
) -> Result<()> {
    access(actor, tenant, user, true)?;
    sqlx::query("UPDATE subscription_connector_runs SET status='canceled',ended_at=now() WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND status IN ('queued','running','retry_wait')").bind(tenant).bind(user).bind(id).execute(pool).await?;
    Ok(())
}
/// One transaction claims an expired lease or a due queued run. Attempts survive process death.
pub async fn claim(pool: &PgPool, owner: &str) -> Result<Option<(String, String, Run)>> {
    let row=sqlx::query("WITH pick AS (SELECT id FROM subscription_connector_runs WHERE (status IN ('queued','retry_wait') AND available_at<=now()) OR (status='running' AND lease_until<now()) ORDER BY id FOR UPDATE SKIP LOCKED LIMIT 1)
        UPDATE subscription_connector_runs r SET status='running',lease_owner=$1,lease_until=now()+interval '90 seconds' FROM pick WHERE r.id=pick.id RETURNING r.tenant_id,r.user_id,r.id")
        .bind(owner).fetch_optional(pool).await?;
    let Some(row) = row else { return Ok(None) };
    let tenant: String = row.try_get("tenant_id")?;
    let user: String = row.try_get("user_id")?;
    let id: i64 = row.try_get("id")?;
    let r = run(
        pool,
        &AdminContext::system_admin("connector-worker"),
        &tenant,
        &user,
        id,
    )
    .await?;
    Ok(Some((tenant, user, r)))
}
pub async fn progress(pool: &PgPool, owner: &str, r: &Run) -> Result<bool> {
    Ok(sqlx::query("UPDATE subscription_connector_runs SET attempts=$3,source_version=$4,error=$5,output_count=$6,lease_until=now()+interval '90 seconds' WHERE id=$1 AND lease_owner=$2 AND status='running'")
        .bind(r.id).bind(owner).bind(json!(r.attempts)).bind(r.source_version).bind(&r.error).bind(r.output_count.map(|v|v as i32)).execute(pool).await?.rows_affected()==1)
}
pub async fn finish(pool: &PgPool, owner: &str, r: &Run, status: &str, retry: bool) -> Result<()> {
    sqlx::query("UPDATE subscription_connector_runs SET status=$3,attempts=$4,error=$5,output_count=$6,source_version=$7,ended_at=CASE WHEN $8 THEN NULL ELSE now() END,available_at=now()+interval '10 seconds',lease_until=NULL WHERE id=$1 AND lease_owner=$2 AND status='running'")
        .bind(r.id).bind(owner).bind(status).bind(json!(r.attempts)).bind(&r.error).bind(r.output_count.map(|v|v as i32)).bind(r.source_version).bind(retry).execute(pool).await?;
    Ok(())
}

/// Atomically confirm the external receipt and complete its worker run while the serving inputs
/// and lease are locked against concurrent changes.
pub async fn confirm_verified(
    pool: &PgPool,
    owner: &str,
    run: &Run,
    tenant: &str,
    user: &str,
    expected_generation: u64,
    expected_agent_public_url: &Option<String>,
    expected_user_uuid: &str,
    fingerprint: &str,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let state = sqlx::query(
        "SELECT serving.generation, control.agent_public_url, users.uuid::text AS user_uuid
           FROM subscription_serving_state serving
           JOIN control_state control ON control.id = TRUE
           JOIN users ON users.tenant_id = $1 AND users.id = $2
          WHERE serving.id = TRUE
          FOR SHARE OF serving, control, users",
    )
    .bind(tenant)
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(state) = state else {
        return Ok(false);
    };
    let generation: i64 = state.try_get("generation")?;
    let agent_public_url: Option<String> = state.try_get("agent_public_url")?;
    let user_uuid: String = state.try_get("user_uuid")?;
    if u64::try_from(generation).ok() != Some(expected_generation)
        || agent_public_url != *expected_agent_public_url
        || user_uuid != expected_user_uuid
    {
        return Ok(false);
    }

    let leased = sqlx::query(
        "SELECT id FROM subscription_connector_runs
          WHERE id = $1 AND tenant_id = $2 AND user_id = $3
            AND status = 'running' AND lease_owner = $4
            AND lease_until > clock_timestamp()
          FOR UPDATE",
    )
    .bind(run.id)
    .bind(tenant)
    .bind(user)
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await?;
    if leased.is_none() {
        return Ok(false);
    }

    let connector_updated = sqlx::query(
        "UPDATE subscription_connectors
            SET verified = TRUE, last_confirmed = now(), source_version = $4,
                source_fingerprint = $5, next_check = now() + interval '5 minutes'
          WHERE tenant_id = $1 AND user_id = $2 AND version = $3
            AND registered = TRUE AND sealed IS NOT NULL",
    )
    .bind(tenant)
    .bind(user)
    .bind(run.config_version)
    .bind(run.source_version)
    .bind(fingerprint)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !connector_updated {
        return Ok(false);
    }

    let run_updated = sqlx::query(
        "UPDATE subscription_connector_runs
            SET status = 'succeeded', attempts = $3, error = NULL, output_count = $4,
                source_version = $5, ended_at = now(), lease_owner = NULL, lease_until = NULL
          WHERE id = $1 AND tenant_id = $6 AND user_id = $7
            AND status = 'running' AND lease_owner = $2
            AND lease_until > clock_timestamp()",
    )
    .bind(run.id)
    .bind(owner)
    .bind(json!(run.attempts))
    .bind(run.output_count.map(|count| count as i32))
    .bind(run.source_version)
    .bind(tenant)
    .bind(user)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !run_updated {
        return Ok(false);
    }

    tx.commit().await?;
    Ok(true)
}

pub async fn due(pool: &PgPool) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as("SELECT c.tenant_id,c.user_id FROM subscription_connectors c JOIN users u ON u.tenant_id=c.tenant_id AND u.id=c.user_id WHERE c.auto AND c.sealed IS NOT NULL AND c.next_check<=now() AND u.status='active' AND NOT u.service_suspended ORDER BY c.next_check LIMIT 32").fetch_all(pool).await?)
}
pub async fn prune(pool: &PgPool) -> Result<()> {
    sqlx::query("DELETE FROM subscription_connector_runs WHERE id IN (SELECT id FROM (SELECT id,ended_at,row_number() OVER (PARTITION BY tenant_id,user_id ORDER BY id DESC) AS n FROM subscription_connector_runs WHERE ended_at IS NOT NULL) r WHERE n>1000 OR ended_at<now()-interval '30 days' LIMIT 1000)").execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthenticatedAdmin, AuthenticatedUser};
    #[test]
    fn self_identity_and_tenant_scope_are_enforced_for_every_operation() {
        let self_actor = AdminContext::from_authenticated(&AuthenticatedAdmin {
            operator_id: "self".into(),
            role: AdminRole::User,
            tenant_scope: Some("platform.acme".into()),
            token_prefix: None,
            self_user: Some(AuthenticatedUser {
                tenant_id: "platform.acme".into(),
                user_id: "alice".into(),
            }),
        });
        for write in [false, true] {
            assert!(access(&self_actor, "platform.acme", "alice", write).is_ok());
            assert!(access(&self_actor, "platform.acme", "bob", write).is_err());
            assert!(access(&self_actor, "other", "alice", write).is_err());
            assert!(access(
                &AdminContext::new("public", AdminRole::Readonly, None),
                "platform.acme",
                "alice",
                write
            )
            .is_err());
        }
        for role in [AdminRole::Readonly, AdminRole::Publisher] {
            let a = AdminContext::new("reviewer", role, None);
            assert!(access(&a, "platform.acme", "alice", false).is_ok());
            assert!(access(&a, "platform.acme", "alice", true).is_err());
        }
        let scoped = AdminContext::new(
            "tenant-admin",
            AdminRole::TenantAdmin,
            Some("platform.acme".into()),
        );
        assert!(access(&scoped, "platform.acme.child", "alice", true).is_ok());
        assert!(access(&scoped, "platform.other", "alice", false).is_err());
    }
}
