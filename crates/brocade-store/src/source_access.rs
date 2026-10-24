//! Durable source denials, serialized policy edits and execution receipts.
use crate::{AdminContext, AdminRole, PgStore, Result, StoreError};
use brocade_deployment::source_access::*;
use sqlx::{PgPool, Row};

fn authorize(
    actor: &AdminContext,
    node: Option<&str>,
    tenant: Option<&str>,
    write: bool,
) -> Result<()> {
    if actor.operator_id() == crate::PUBLIC_OPERATOR_ID {
        return Err(StoreError::Forbidden("来源封禁不向访客开放".into()));
    }
    if node.is_some() {
        if !actor.is_system_admin() {
            return Err(StoreError::Forbidden(
                "节点来源封禁需要系统管理员权限".into(),
            ));
        }
    } else {
        actor.require_tenant_access(
            tenant.ok_or_else(|| StoreError::InvalidData("missing tenant".into()))?,
            "source access",
        )?;
        if !matches!(
            actor.role(),
            AdminRole::SystemAdmin
                | AdminRole::TenantAdmin
                | AdminRole::Editor
                | AdminRole::Publisher
                | AdminRole::Readonly
        ) {
            return Err(StoreError::Forbidden("来源封禁仅对管理账号开放".into()));
        }
    }
    if write
        && !matches!(
            actor.role(),
            AdminRole::SystemAdmin
                | AdminRole::TenantAdmin
                | AdminRole::Editor
                | AdminRole::Publisher
        )
    {
        return Err(StoreError::Forbidden("修改来源封禁需要编辑权限".into()));
    }
    Ok(())
}

fn scope(node: Option<&str>, tenant: Option<&str>, user: Option<&str>) -> Result<String> {
    match (node, tenant, user) {
        (Some(node), None, None) => Ok(format!("node:{node}")),
        (None, Some(tenant), Some(user)) => Ok(format!("user:{tenant}/{user}")),
        _ => Err(StoreError::InvalidData(
            "invalid source access scope".into(),
        )),
    }
}

async fn exists(
    pool: &PgPool,
    node: Option<&str>,
    tenant: Option<&str>,
    user: Option<&str>,
) -> Result<()> {
    let exists: bool = if let Some(node) = node {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes WHERE id=$1)")
            .bind(node)
            .fetch_one(pool)
            .await?
    } else {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2)")
            .bind(tenant)
            .bind(user)
            .fetch_one(pool)
            .await?
    };
    if !exists {
        return Err(StoreError::NotFound("source access owner".into()));
    }
    Ok(())
}

impl PgStore {
    pub async fn source_access_revision(&self) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT revision FROM source_access_state WHERE singleton")
                .fetch_one(self.pool())
                .await?,
        )
    }
    pub async fn source_access_view(
        &self,
        actor: &AdminContext,
        node: Option<&str>,
        tenant: Option<&str>,
        user: Option<&str>,
    ) -> Result<SourceAccessView> {
        authorize(actor, node, tenant, false)?;
        let key = scope(node, tenant, user)?;
        exists(self.pool(), node, tenant, user).await?;
        let policy =
            sqlx::query("SELECT revision,ips FROM source_access_policies WHERE scope_key=$1")
                .bind(key)
                .fetch_optional(self.pool())
                .await?;
        let (revision, ips) = match policy {
            Some(row) => (
                row.try_get::<i64, _>("revision")?,
                serde_json::from_value::<Vec<String>>(row.try_get("ips")?)?,
            ),
            None => (0, Vec::new()),
        };
        // User denials are delivered to infrastructure nodes so new grants and draining
        // generations cannot temporarily escape a ban. Only relevant nodes are displayed.
        let rows=sqlx::query("SELECT n.id,s.revision,s.supported,s.error,s.reported_at>now()-interval '30 seconds' AS fresh,
                a.agent_protocol_version AS protocol
            FROM nodes n LEFT JOIN source_access_node_state s ON s.node_id=n.id
            LEFT JOIN node_agent_state a ON a.node_id=n.id WHERE n.retired_at IS NULL AND
            (($1::text IS NOT NULL AND n.id=$1) OR ($1::text IS NULL AND (
                EXISTS(SELECT 1 FROM grants g JOIN ingresses i ON i.app_id=g.app_id AND i.id=g.ingress_id WHERE g.tenant_id=$2 AND g.user_id=$3 AND i.node_id=n.id)
                OR EXISTS(SELECT 1 FROM quota_suspensions q JOIN ingresses i ON i.app_id=q.app_id AND i.id=q.ingress_id WHERE q.tenant_id=$2 AND q.user_id=$3 AND i.node_id=n.id)))) ORDER BY n.id")
            .bind(node).bind(tenant).bind(user).fetch_all(self.pool()).await?;
        let nodes = rows
            .into_iter()
            .map(|row| -> Result<SourceAccessNode> {
                let supported = row.try_get::<Option<bool>, _>("supported")?;
                let fresh = row.try_get::<Option<bool>, _>("fresh")?.unwrap_or(false);
                let applied = row
                    .try_get::<Option<i64>, _>("revision")?
                    .is_some_and(|r| r >= revision);
                let protocol = row.try_get::<Option<i32>, _>("protocol")?;
                let phase = if protocol
                    .is_some_and(|version| version < SOURCE_ACCESS_PROTOCOL_VERSION as i32)
                {
                    SourceAccessPhase::Unsupported
                } else if protocol.is_none() || !fresh {
                    SourceAccessPhase::Pending
                } else if supported == Some(false) {
                    SourceAccessPhase::Unsupported
                } else if row.try_get::<Option<String>, _>("error")?.is_some() {
                    SourceAccessPhase::Error
                } else if applied {
                    SourceAccessPhase::Applied
                } else {
                    SourceAccessPhase::Pending
                };
                Ok(SourceAccessNode {
                    node_id: row.try_get("id")?,
                    phase,
                    error: row.try_get("error")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(SourceAccessView {
            revision,
            ips,
            nodes,
        })
    }

    pub async fn update_source_access(
        &self,
        actor: &AdminContext,
        node: Option<&str>,
        tenant: Option<&str>,
        user: Option<&str>,
        request: SourceAccessWrite,
    ) -> Result<SourceAccessView> {
        authorize(actor, node, tenant, true)?;
        let key = scope(node, tenant, user)?;
        let ips = request.normalized().map_err(StoreError::InvalidData)?;
        let mut tx = self.pool().begin().await?;
        // Parent before generation: owner deletion cascades into this policy and
        // takes the generation lock in its delete trigger in the same order.
        let owner = if let Some(node) = node {
            sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR KEY SHARE")
                .bind(node)
                .fetch_optional(&mut *tx)
                .await?
        } else {
            sqlx::query("SELECT id FROM users WHERE tenant_id=$1 AND id=$2 FOR KEY SHARE")
                .bind(tenant)
                .bind(user)
                .fetch_optional(&mut *tx)
                .await?
        };
        if owner.is_none() {
            return Err(StoreError::NotFound("source access owner".into()));
        }
        // This lock also gives a snapshot one generation across all policy rows.
        sqlx::query("SELECT revision FROM source_access_state WHERE singleton FOR UPDATE")
            .fetch_one(&mut *tx)
            .await?;
        let old = sqlx::query("SELECT revision,ips FROM source_access_policies WHERE scope_key=$1")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?;
        let (old_revision, old_ips) = match old {
            Some(row) => (
                row.try_get::<i64, _>("revision")?,
                row.try_get::<serde_json::Value, _>("ips")?,
            ),
            None => (0, serde_json::json!([])),
        };
        if old_revision != request.revision {
            return Err(StoreError::Conflict(
                "封禁名单已被其他操作修改，请刷新后重试".into(),
            ));
        }
        let json = serde_json::to_value(&ips)?;
        if old_ips != json {
            let (total, bytes):(i64,i64)=sqlx::query_as("SELECT COALESCE(sum(jsonb_array_length(ips)),0)::bigint,
                COALESCE(sum(octet_length(ips::text)+COALESCE(octet_length(tenant_id),0)+COALESCE(octet_length(user_id),0)+128),0)::bigint
                FROM source_access_policies WHERE scope_key<>$1 AND jsonb_array_length(ips)>0").bind(&key).fetch_one(&mut *tx).await?;
            let bytes = usize::try_from(bytes).map_err(|_| {
                StoreError::InvalidData("invalid source access snapshot size".into())
            })?;
            let added_bytes = if ips.is_empty() {
                0
            } else {
                serde_json::to_vec(&ips)?.len()
                    + tenant.map_or(0, str::len)
                    + user.map_or(0, str::len)
                    + 128
            };
            if bytes + added_bytes + 1024 > MAX_SOURCE_ACCESS_BYTES {
                return Err(StoreError::InvalidData(
                    "来源封禁超过节点同步容量（2 MiB）".into(),
                ));
            }
            if total + ips.len() as i64 > MAX_SOURCE_ACCESS_ENTRIES as i64 {
                return Err(StoreError::InvalidData("来源封禁总数已达上限".into()));
            }
            let revision:i64=sqlx::query_scalar("UPDATE source_access_state SET revision=revision+1 WHERE singleton RETURNING revision").fetch_one(&mut *tx).await?;
            sqlx::query("INSERT INTO source_access_policies(scope_key,tenant_id,user_id,node_id,revision,ips,updated_by) VALUES($1,$2,$3,$4,$5,$6,$7)
                ON CONFLICT(scope_key) DO UPDATE SET revision=$5,ips=$6,updated_by=$7,updated_at=now()")
                .bind(&key).bind(tenant).bind(user).bind(node).bind(revision).bind(&json).bind(actor.operator_id()).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO source_access_events(scope_key,revision,ips,actor) VALUES($1,$2,$3,$4)").bind(&key).bind(revision).bind(json).bind(actor.operator_id()).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.source_access_view(actor, node, tenant, user).await
    }

    pub async fn source_access_desired(&self, node: &str) -> Result<SourceAccessDesired> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let revision =
            sqlx::query_scalar("SELECT revision FROM source_access_state WHERE singleton")
                .fetch_one(&mut *tx)
                .await?;
        let rows=sqlx::query("SELECT tenant_id,user_id,node_id,ips FROM source_access_policies WHERE node_id=$1 OR user_id IS NOT NULL ORDER BY scope_key COLLATE \"C\"")
            .bind(node).fetch_all(&mut *tx).await?;
        let mut desired = SourceAccessDesired {
            revision,
            ..Default::default()
        };
        for row in rows {
            let ips: Vec<String> = serde_json::from_value(row.try_get("ips")?)?;
            if row.try_get::<Option<String>, _>("node_id")?.is_some() {
                desired.node_ips = ips;
            } else if !ips.is_empty() {
                desired.users.push(SourceAccessUser {
                    tenant_id: row.try_get("tenant_id")?,
                    user_id: row.try_get("user_id")?,
                    ips,
                });
            }
        }
        desired.validate().map_err(StoreError::InvalidData)?;
        tx.commit().await?;
        Ok(desired)
    }

    pub async fn report_source_access(
        &self,
        node: &str,
        report: SourceAccessObservation,
    ) -> Result<()> {
        if report.revision < 0
            || report
                .error
                .as_ref()
                .is_some_and(|e| e.chars().count() > 512)
        {
            return Err(StoreError::InvalidData(
                "invalid source access observation".into(),
            ));
        }
        sqlx::query("INSERT INTO source_access_node_state(node_id,revision,supported,error) SELECT $1,$2,$3,$4 WHERE $2<=(SELECT revision FROM source_access_state WHERE singleton)
            ON CONFLICT(node_id) DO UPDATE SET revision=$2,supported=$3,error=$4,reported_at=now() WHERE source_access_node_state.revision<=$2")
            .bind(node).bind(report.revision).bind(report.supported).bind(report.error).execute(self.pool()).await?;
        Ok(())
    }
}
