use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

#[cfg(test)]
use brocade_core::forwarding::target_ip_is_blocked as address_is_private;
use brocade_core::forwarding::target_ip_is_blocked_by_policy;
use brocade_deployment::protocol::{
    ForwardingDesiredState, ForwardingProtocol, ForwardingReconcileReport, ForwardingRuleDesired,
    ForwardingRuleReconcileResult, ForwardingUsageReportRequest, ForwardingUsageReportResult,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, AdminRole, Result, StoreError};

pub const DEFAULT_FORWARDING_PORT_START: u16 = 30_000;
pub const DEFAULT_FORWARDING_PORT_END: u16 = 59_999;
pub const DEFAULT_FORWARDING_MAX_RULES: u16 = 100;
const MAX_USAGE_COUNTERS: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardingEntitlementWrite {
    pub enabled: bool,
    #[serde(default = "default_tcp_allowed")]
    pub tcp_allowed: bool,
    #[serde(default = "default_udp_allowed")]
    pub udp_allowed: bool,
    #[serde(default = "default_port_start")]
    pub listen_port_start: u16,
    #[serde(default = "default_port_end")]
    pub listen_port_end: u16,
    #[serde(default = "default_max_rules")]
    pub max_rules: u16,
    #[serde(default = "default_allow_private_targets")]
    pub allow_private_targets: bool,
}

fn default_tcp_allowed() -> bool {
    true
}
fn default_udp_allowed() -> bool {
    true
}
fn default_port_start() -> u16 {
    DEFAULT_FORWARDING_PORT_START
}
fn default_port_end() -> u16 {
    DEFAULT_FORWARDING_PORT_END
}
fn default_max_rules() -> u16 {
    DEFAULT_FORWARDING_MAX_RULES
}
fn default_allow_private_targets() -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardingEntitlementView {
    pub node_id: String,
    pub node_name: String,
    pub public_ipv4: Option<String>,
    pub public_ipv6: Option<String>,
    pub enabled: bool,
    pub tcp_allowed: bool,
    pub udp_allowed: bool,
    pub listen_port_start: u16,
    pub listen_port_end: u16,
    pub max_rules: u16,
    pub allow_private_targets: bool,
    pub rule_count: u16,
    pub desired_generation: i64,
    pub applied_generation: Option<i64>,
    pub reconcile_status: String,
    pub last_error: Option<String>,
    pub reported_at: Option<String>,
    pub cycle_limit_bytes: Option<u64>,
    pub cycle_used_bytes: u64,
    pub quota_suspended: bool,
    pub quota_job_id: Option<i64>,
    pub quota_job_status: Option<String>,
    pub quota_job_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardingRuleWrite {
    pub node_id: String,
    pub name: String,
    pub protocol: ForwardingProtocol,
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardingRuleView {
    pub id: i64,
    pub tenant_id: String,
    pub user_id: String,
    pub node_id: String,
    pub name: String,
    pub protocol: ForwardingProtocol,
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
    pub enabled: bool,
    pub runtime_status: String,
    pub runtime_error: Option<String>,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardingOverview {
    pub tenant_id: String,
    pub user_id: String,
    pub entitlements: Vec<ForwardingEntitlementView>,
    pub rules: Vec<ForwardingRuleView>,
}

pub(crate) fn require_user_access(
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<()> {
    if actor.role() == AdminRole::User {
        let own = actor
            .self_user()
            .is_some_and(|user| user.tenant_id == tenant_id && user.user_id == user_id);
        if own {
            return Ok(());
        }
        return Err(StoreError::Forbidden(
            "users may manage only their own forwarding rules".to_owned(),
        ));
    }
    actor.require_tenant_access(tenant_id, "forwarding user")
}

pub(crate) fn require_rule_write_access(
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<()> {
    if actor.role() == AdminRole::User {
        return require_user_access(actor, tenant_id, user_id);
    }
    if !matches!(
        actor.role(),
        AdminRole::Editor | AdminRole::Publisher | AdminRole::TenantAdmin | AdminRole::SystemAdmin
    ) {
        return Err(StoreError::Forbidden(
            "forwarding rules require editor access".to_owned(),
        ));
    }
    actor.require_tenant_access(tenant_id, "forwarding rule")
}

pub(crate) fn require_entitlement_admin(actor: &AdminContext, tenant_id: &str) -> Result<()> {
    if !matches!(
        actor.role(),
        AdminRole::TenantAdmin | AdminRole::SystemAdmin
    ) {
        return Err(StoreError::Forbidden(
            "only tenant-admin or system-admin can grant forwarding entry nodes".to_owned(),
        ));
    }
    actor.require_tenant_access(tenant_id, "forwarding entitlement")
}

fn protocol_name(protocol: ForwardingProtocol) -> &'static str {
    match protocol {
        ForwardingProtocol::Tcp => "tcp",
        ForwardingProtocol::Udp => "udp",
        ForwardingProtocol::TcpUdp => "tcp_udp",
    }
}

fn parse_protocol(value: &str) -> Result<ForwardingProtocol> {
    match value {
        "tcp" => Ok(ForwardingProtocol::Tcp),
        "udp" => Ok(ForwardingProtocol::Udp),
        "tcp_udp" => Ok(ForwardingProtocol::TcpUdp),
        other => Err(StoreError::InvalidData(format!(
            "unknown forwarding protocol {other}"
        ))),
    }
}

fn validate_entitlement(write: &ForwardingEntitlementWrite) -> Result<()> {
    if !write.tcp_allowed && !write.udp_allowed {
        return Err(StoreError::InvalidData(
            "at least one forwarding protocol must be allowed".to_owned(),
        ));
    }
    if write.listen_port_start < 1024 || write.listen_port_start > write.listen_port_end {
        return Err(StoreError::InvalidData(
            "forwarding listen port range must be within 1024..65535".to_owned(),
        ));
    }
    if write.max_rules == 0 || write.max_rules > 256 {
        return Err(StoreError::InvalidData(
            "forwarding max_rules must be within 1..256".to_owned(),
        ));
    }
    Ok(())
}

fn validate_rule_shape(write: &ForwardingRuleWrite) -> Result<()> {
    let name = write.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(StoreError::InvalidData(
            "forwarding rule name must contain 1..80 characters".to_owned(),
        ));
    }
    let host = write.target_host.trim();
    if host.is_empty() || host.len() > 253 || host.chars().any(char::is_whitespace) {
        return Err(StoreError::InvalidData(
            "forwarding target host is invalid".to_owned(),
        ));
    }
    if write.listen_port == 0 || write.target_port == 0 {
        return Err(StoreError::InvalidData(
            "forwarding ports must be within 1..65535".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) async fn bump_node_generation(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "INSERT INTO forwarding_node_state (node_id, desired_generation, reconcile_status)
         VALUES ($1, 1, 'pending')
         ON CONFLICT (node_id) DO UPDATE SET
             desired_generation = forwarding_node_state.desired_generation + 1,
             reconcile_status = 'pending',
             last_error = NULL
         RETURNING desired_generation",
    )
    .bind(node_id)
    .fetch_one(&mut **tx)
    .await?)
}

async fn lock_entitlement(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    user_id: &str,
    node_id: &str,
) -> Result<()> {
    let _: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM forwarding_entitlements
          WHERE tenant_id = $1 AND user_id = $2 AND node_id = $3
          FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(())
}

pub async fn set_entitlement(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    node_id: &str,
    write: ForwardingEntitlementWrite,
) -> Result<ForwardingOverview> {
    require_entitlement_admin(actor, tenant_id)?;
    validate_entitlement(&write)?;
    let mut tx = pool.begin().await?;
    crate::user_traffic::lock_user_tx(&mut tx, tenant_id, user_id).await?;
    // Keep ownership stable through the grant: a concurrent tenant transfer must not let an
    // authorization based on the previous owner commit against the new owner's node.
    let node_tenant: Option<String> = sqlx::query_scalar(
        "SELECT n.tenant_id
           FROM users u CROSS JOIN nodes n
          WHERE u.tenant_id = $1 AND u.id = $2
            AND n.id = $3 AND n.retired_at IS NULL AND n.api_port IS NOT NULL
          FOR SHARE OF n",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?;
    let node_tenant = node_tenant.ok_or_else(|| {
        StoreError::NotFound("user or active Xray-capable node was not found".to_owned())
    })?;
    actor.require_tenant_access(&node_tenant, "forwarding entry node")?;
    // Serialize policy edits with rule creation/update. Otherwise a rule could pass validation
    // against the old policy while an administrator narrows the same entitlement.
    lock_entitlement(&mut tx, tenant_id, user_id, node_id).await?;
    if write.enabled {
        crate::user_traffic::lock_user_tx(&mut tx, tenant_id, user_id).await?;
        let invalid_rules: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM forwarding_rules
              WHERE tenant_id = $1 AND user_id = $2 AND node_id = $3
                AND deleted_at IS NULL AND enabled
                AND (
                    listen_port NOT BETWEEN $4 AND $5 OR
                    (protocol IN ('tcp', 'tcp_udp') AND NOT $6) OR
                    (protocol IN ('udp', 'tcp_udp') AND NOT $7)
                )",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(node_id)
        .bind(i32::from(write.listen_port_start))
        .bind(i32::from(write.listen_port_end))
        .bind(write.tcp_allowed)
        .bind(write.udp_allowed)
        .fetch_one(&mut *tx)
        .await?;
        if invalid_rules > 0 {
            return Err(StoreError::Conflict(format!(
                "update or disable {invalid_rules} active forwarding rule(s) before narrowing the authorization"
            )));
        }
        let existing_rules: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM forwarding_rules
              WHERE tenant_id = $1 AND user_id = $2 AND node_id = $3
                AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
        if existing_rules > i64::from(write.max_rules) {
            return Err(StoreError::Conflict(format!(
                "remove rules before lowering the forwarding rule limit below {existing_rules}"
            )));
        }
    }
    sqlx::query(
        "INSERT INTO forwarding_entitlements (
             tenant_id, user_id, node_id, tcp_allowed, udp_allowed,
             listen_port_start, listen_port_end, max_rules, allow_private_targets,
             enabled, updated_by
         ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
         ON CONFLICT (tenant_id, user_id, node_id) DO UPDATE SET
             tcp_allowed = EXCLUDED.tcp_allowed,
             udp_allowed = EXCLUDED.udp_allowed,
             listen_port_start = EXCLUDED.listen_port_start,
             listen_port_end = EXCLUDED.listen_port_end,
             max_rules = EXCLUDED.max_rules,
             allow_private_targets = EXCLUDED.allow_private_targets,
             enabled = EXCLUDED.enabled,
             updated_by = EXCLUDED.updated_by,
             updated_at = now()",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .bind(write.tcp_allowed)
    .bind(write.udp_allowed)
    .bind(i32::from(write.listen_port_start))
    .bind(i32::from(write.listen_port_end))
    .bind(i32::from(write.max_rules))
    .bind(write.allow_private_targets)
    .bind(write.enabled)
    .bind(actor.operator_id())
    .execute(&mut *tx)
    .await?;
    disable_blocked_rules(&mut tx, node_id, Some((tenant_id, user_id))).await?;
    bump_node_generation(&mut tx, node_id).await?;
    tx.commit().await?;
    overview(pool, actor, tenant_id, user_id).await
}

pub async fn delete_entitlement(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    node_id: &str,
) -> Result<ForwardingOverview> {
    require_entitlement_admin(actor, tenant_id)?;
    let mut tx = pool.begin().await?;
    let enabled: Option<bool> = sqlx::query_scalar(
        "SELECT enabled FROM forwarding_entitlements
          WHERE tenant_id = $1 AND user_id = $2 AND node_id = $3
          FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?;
    let enabled =
        enabled.ok_or_else(|| StoreError::NotFound("forwarding entitlement".to_owned()))?;
    if enabled {
        return Err(StoreError::Conflict(
            "revoke the forwarding entitlement before removing it".to_owned(),
        ));
    }

    // Removing a revoked entitlement must also retire its rules. Leaving enabled rule rows behind
    // would make a later grant for the same user and node silently reactivate old listeners.
    // Soft deletion preserves rule identity and historical usage while freeing the listen ports.
    sqlx::query(
        "WITH locked_rules AS MATERIALIZED (
             SELECT id FROM forwarding_rules
              WHERE tenant_id = $1 AND user_id = $2 AND node_id = $3 AND deleted_at IS NULL
              ORDER BY id FOR UPDATE
         )
         UPDATE forwarding_rules r
            SET deleted_at = now(), enabled = false, updated_at = now()
           FROM locked_rules
          WHERE r.id = locked_rules.id",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM forwarding_entitlements
          WHERE tenant_id = $1 AND user_id = $2 AND node_id = $3",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .execute(&mut *tx)
    .await?;
    bump_node_generation(&mut tx, node_id).await?;
    crate::forwarding_quota::cancel_jobs_tx(&mut tx, tenant_id, user_id, node_id).await?;
    tx.commit().await?;
    overview(pool, actor, tenant_id, user_id).await
}

pub async fn overview(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<ForwardingOverview> {
    require_user_access(actor, tenant_id, user_id)?;
    let rows = sqlx::query(&format!(
        "SELECT e.node_id, n.name AS node_name, n.public_ipv4, n.public_ipv6,
                e.enabled, e.tcp_allowed, e.udp_allowed, e.listen_port_start,
                e.listen_port_end, e.max_rules, e.allow_private_targets,
                count(r.id) FILTER (WHERE r.deleted_at IS NULL)::bigint AS rule_count,
                COALESCE(s.desired_generation, 0) AS desired_generation,
                s.applied_generation, COALESCE(s.reconcile_status, 'pending') AS reconcile_status,
                s.last_error, s.reported_at::text AS reported_at, q.limit_bytes AS cycle_limit_bytes,
                {cycle_used} AS cycle_used_bytes, COALESCE(q.suspended,false) AS quota_suspended,
                q.last_job_id AS quota_job_id, j.status AS quota_job_status, j.last_error AS quota_job_error
           FROM forwarding_entitlements e
           JOIN nodes n ON n.id = e.node_id
           LEFT JOIN forwarding_rules r
             ON r.tenant_id = e.tenant_id AND r.user_id = e.user_id AND r.node_id = e.node_id
           LEFT JOIN forwarding_node_state s ON s.node_id = e.node_id
           LEFT JOIN forwarding_node_quotas q ON q.tenant_id=e.tenant_id AND q.user_id=e.user_id AND q.node_id=e.node_id
           LEFT JOIN jobs j ON j.id=q.last_job_id
          WHERE e.tenant_id = $1 AND e.user_id = $2
          GROUP BY e.tenant_id,e.user_id,e.node_id, n.name, n.public_ipv4, n.public_ipv6,
                   e.enabled, e.tcp_allowed, e.udp_allowed, e.listen_port_start,
                   e.listen_port_end, e.max_rules, e.allow_private_targets,
                   s.desired_generation, s.applied_generation, s.reconcile_status, s.last_error, s.reported_at,
                   q.tenant_id,q.user_id,q.node_id,j.id
          ORDER BY n.name, e.node_id", cycle_used=crate::forwarding_quota::CYCLE_USED
    ))
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let entitlements = rows
        .into_iter()
        .map(|row| {
            Ok(ForwardingEntitlementView {
                node_id: row.try_get("node_id")?,
                node_name: row.try_get("node_name")?,
                public_ipv4: row.try_get("public_ipv4")?,
                public_ipv6: row.try_get("public_ipv6")?,
                enabled: row.try_get("enabled")?,
                tcp_allowed: row.try_get("tcp_allowed")?,
                udp_allowed: row.try_get("udp_allowed")?,
                listen_port_start: integer_to_u16(row.try_get("listen_port_start")?)?,
                listen_port_end: integer_to_u16(row.try_get("listen_port_end")?)?,
                max_rules: integer_to_u16(row.try_get("max_rules")?)?,
                allow_private_targets: row.try_get("allow_private_targets")?,
                rule_count: i64_to_u16(row.try_get("rule_count")?)?,
                desired_generation: row.try_get("desired_generation")?,
                applied_generation: row.try_get("applied_generation")?,
                reconcile_status: row.try_get("reconcile_status")?,
                last_error: row.try_get("last_error")?,
                reported_at: row.try_get("reported_at")?,
                cycle_limit_bytes: row
                    .try_get::<Option<i64>, _>("cycle_limit_bytes")?
                    .map(|n| {
                        u64::try_from(n).map_err(|_| {
                            StoreError::InvalidData("negative forwarding quota".to_owned())
                        })
                    })
                    .transpose()?,
                cycle_used_bytes: u64::try_from(row.try_get::<i64, _>("cycle_used_bytes")?)
                    .map_err(|_| StoreError::InvalidData("negative forwarding usage".to_owned()))?,
                quota_suspended: row.try_get("quota_suspended")?,
                quota_job_id: row.try_get("quota_job_id")?,
                quota_job_status: row.try_get("quota_job_status")?,
                quota_job_error: row.try_get("quota_job_error")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let rows = sqlx::query(
        "SELECT r.id, r.tenant_id, r.user_id, r.node_id, r.name, r.protocol,
                r.listen_port, r.target_host, r.target_port, r.enabled,
                r.created_at::text AS created_at, r.updated_at::text AS updated_at,
                COALESCE(sum(s.uplink_bytes), 0)::bigint AS uplink_bytes,
                COALESCE(sum(s.downlink_bytes), 0)::bigint AS downlink_bytes,
                CASE
                  WHEN NOT e.enabled OR NOT r.enabled OR owner.status <> 'active' THEN 'disabled'
                  WHEN owner.service_suspended THEN 'service-paused'
                  WHEN COALESCE(q.suspended,false) THEN 'quota-paused'
                  WHEN state.reported_at < now() - interval '90 seconds' THEN 'unknown'
                  WHEN result.value->'rule' = jsonb_build_object(
                      'id',r.id,'protocol',r.protocol,'listen_port',r.listen_port,
                      'target_host',r.target_host,'target_port',r.target_port,
                      'allow_private_targets',e.allow_private_targets)
                    THEN CASE
                      WHEN (result.value->>'applied')::boolean THEN 'active'
                      WHEN result.value->>'error' IS NOT NULL THEN 'error'
                      ELSE 'pending'
                    END
                  WHEN state.rule_results IS NULL AND state.reconcile_status = 'error' THEN 'unknown'
                  WHEN state.rule_results IS NULL AND state.applied_generation = state.desired_generation
                       AND state.applied_rule_ids @> jsonb_build_array(r.id) THEN 'active'
                  ELSE 'pending'
                END AS runtime_status,
                CASE WHEN result.value->'rule' = jsonb_build_object(
                    'id',r.id,'protocol',r.protocol,'listen_port',r.listen_port,
                    'target_host',r.target_host,'target_port',r.target_port,
                      'allow_private_targets',e.allow_private_targets)
                  THEN result.value->>'error' END AS runtime_error
           FROM forwarding_rules r
           JOIN forwarding_entitlements e
             ON e.tenant_id = r.tenant_id AND e.user_id = r.user_id AND e.node_id = r.node_id
           JOIN users owner ON owner.tenant_id=r.tenant_id AND owner.id=r.user_id
           LEFT JOIN forwarding_node_state state ON state.node_id = r.node_id
           LEFT JOIN LATERAL (SELECT state.rule_results->r.id::text AS value) result ON TRUE
           LEFT JOIN forwarding_node_quotas q ON q.tenant_id=r.tenant_id AND q.user_id=r.user_id AND q.node_id=r.node_id
           LEFT JOIN forwarding_usage_rollups s ON s.rule_id = r.id
          WHERE r.tenant_id = $1 AND r.user_id = $2 AND r.deleted_at IS NULL
          GROUP BY r.id, e.enabled, e.allow_private_targets, owner.status, owner.service_suspended, q.suspended, state.reconcile_status,
                   state.applied_generation, state.desired_generation, state.applied_rule_ids,
                   state.rule_results, state.reported_at, result.value
          ORDER BY r.created_at, r.id",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let rules = rows
        .into_iter()
        .map(rule_view_from_row)
        .collect::<Result<Vec<_>>>()?;
    Ok(ForwardingOverview {
        tenant_id: tenant_id.to_owned(),
        user_id: user_id.to_owned(),
        entitlements,
        rules,
    })
}

pub async fn create_rule(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    write: ForwardingRuleWrite,
) -> Result<ForwardingOverview> {
    require_rule_write_access(actor, tenant_id, user_id)?;
    validate_rule_shape(&write)?;
    let mut tx = pool.begin().await?;
    crate::user_traffic::lock_user_tx(&mut tx, tenant_id, user_id).await?;
    validate_rule_against_entitlement(&mut tx, tenant_id, user_id, &write, None).await?;
    let result = sqlx::query(
        "INSERT INTO forwarding_rules (
             tenant_id, user_id, node_id, name, protocol, listen_port,
             target_host, target_port, enabled
         ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
         RETURNING id",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(&write.node_id)
    .bind(write.name.trim())
    .bind(protocol_name(write.protocol))
    .bind(i32::from(write.listen_port))
    .bind(write.target_host.trim())
    .bind(i32::from(write.target_port))
    .bind(write.enabled)
    .fetch_one(&mut *tx)
    .await;
    let row = map_port_conflict(result)?;
    remember_rule_node(&mut tx, row.try_get("id")?, &write.node_id).await?;
    bump_node_generation(&mut tx, &write.node_id).await?;
    tx.commit().await?;
    overview(pool, actor, tenant_id, user_id).await
}

pub async fn update_rule(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    rule_id: i64,
    write: ForwardingRuleWrite,
) -> Result<ForwardingOverview> {
    require_rule_write_access(actor, tenant_id, user_id)?;
    validate_rule_shape(&write)?;
    let mut tx = pool.begin().await?;
    crate::user_traffic::lock_user_tx(&mut tx, tenant_id, user_id).await?;
    // Policy edits and entitlement deletion take the grant before its rules. Acquire the
    // destination grant in the same order; validate its current policy after locking the rule.
    lock_entitlement(&mut tx, tenant_id, user_id, &write.node_id).await?;
    let previous = sqlx::query(
        "SELECT node_id,protocol,listen_port,target_host,target_port,enabled FROM forwarding_rules
          WHERE id = $1 AND tenant_id = $2 AND user_id = $3 AND deleted_at IS NULL
          FOR UPDATE",
    )
    .bind(rule_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let previous = previous.ok_or_else(|| StoreError::NotFound("forwarding rule".to_owned()))?;
    let old_node: String = previous.try_get("node_id")?;
    let runtime_changed = old_node != write.node_id
        || previous.try_get::<String, _>("protocol")? != protocol_name(write.protocol)
        || previous.try_get::<i32, _>("listen_port")? != i32::from(write.listen_port)
        || previous.try_get::<String, _>("target_host")? != write.target_host.trim()
        || previous.try_get::<i32, _>("target_port")? != i32::from(write.target_port)
        || previous.try_get::<bool, _>("enabled")? != write.enabled;
    remember_rule_node(&mut tx, rule_id, &old_node).await?;
    if runtime_changed && write.enabled {
        validate_rule_against_entitlement(&mut tx, tenant_id, user_id, &write, Some(rule_id))
            .await?;
    }
    let result = sqlx::query(
        "UPDATE forwarding_rules SET
             node_id = $4, name = $5, protocol = $6, listen_port = $7,
             target_host = $8, target_port = $9, enabled = $10, updated_at = now()
          WHERE id = $1 AND tenant_id = $2 AND user_id = $3",
    )
    .bind(rule_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&write.node_id)
    .bind(write.name.trim())
    .bind(protocol_name(write.protocol))
    .bind(i32::from(write.listen_port))
    .bind(write.target_host.trim())
    .bind(i32::from(write.target_port))
    .bind(write.enabled)
    .execute(&mut *tx)
    .await;
    map_port_conflict(result)?;
    remember_rule_node(&mut tx, rule_id, &write.node_id).await?;
    if runtime_changed {
        // Opposite moves and quota batches must acquire node states in the same order.
        for node in BTreeSet::from([old_node.as_str(), write.node_id.as_str()]) {
            bump_node_generation(&mut tx, node).await?;
        }
    }
    tx.commit().await?;
    overview(pool, actor, tenant_id, user_id).await
}

pub async fn delete_rule(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    rule_id: i64,
) -> Result<ForwardingOverview> {
    require_rule_write_access(actor, tenant_id, user_id)?;
    let mut tx = pool.begin().await?;
    let node_id: Option<String> = sqlx::query_scalar(
        "UPDATE forwarding_rules
            SET deleted_at = now(), enabled = false, updated_at = now()
          WHERE id = $1 AND tenant_id = $2 AND user_id = $3 AND deleted_at IS NULL
          RETURNING node_id",
    )
    .bind(rule_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let node_id = node_id.ok_or_else(|| StoreError::NotFound("forwarding rule".to_owned()))?;
    bump_node_generation(&mut tx, &node_id).await?;
    tx.commit().await?;
    overview(pool, actor, tenant_id, user_id).await
}

async fn validate_rule_against_entitlement(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    user_id: &str,
    write: &ForwardingRuleWrite,
    exclude_rule: Option<i64>,
) -> Result<()> {
    let row = sqlx::query(
        "SELECT e.tcp_allowed, e.udp_allowed, e.listen_port_start, e.listen_port_end,
                e.max_rules, e.allow_private_targets, e.enabled, u.status,
                (SELECT count(*) FROM forwarding_rules r
                  WHERE r.tenant_id = e.tenant_id AND r.user_id = e.user_id
                    AND r.node_id = e.node_id AND r.deleted_at IS NULL
                    AND ($4::bigint IS NULL OR r.id <> $4))::bigint AS rule_count
           FROM forwarding_entitlements e
           JOIN users u ON u.tenant_id = e.tenant_id AND u.id = e.user_id
           JOIN nodes n ON n.id = e.node_id AND n.retired_at IS NULL AND n.api_port IS NOT NULL
          WHERE e.tenant_id = $1 AND e.user_id = $2 AND e.node_id = $3
          FOR UPDATE OF e",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(&write.node_id)
    .bind(exclude_rule)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::Forbidden("forwarding node is not authorized".to_owned()))?;
    if !row.try_get::<bool, _>("enabled")? || row.try_get::<String, _>("status")? != "active" {
        return Err(StoreError::Forbidden(
            "forwarding entitlement or user is disabled".to_owned(),
        ));
    }
    let tcp = row.try_get::<bool, _>("tcp_allowed")?;
    let udp = row.try_get::<bool, _>("udp_allowed")?;
    let allowed = match write.protocol {
        ForwardingProtocol::Tcp => tcp,
        ForwardingProtocol::Udp => udp,
        ForwardingProtocol::TcpUdp => tcp && udp,
    };
    if !allowed {
        return Err(StoreError::Forbidden(
            "forwarding protocol is not authorized".to_owned(),
        ));
    }
    let start: i32 = row.try_get("listen_port_start")?;
    let end: i32 = row.try_get("listen_port_end")?;
    if i32::from(write.listen_port) < start || i32::from(write.listen_port) > end {
        return Err(StoreError::InvalidData(format!(
            "listen port must be within {start}..{end}"
        )));
    }
    if write.enabled {
        crate::user_traffic::lock_user_tx(tx, tenant_id, user_id).await?;
        let exhausted: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM forwarding_entitlements e
            JOIN forwarding_node_quotas q USING(tenant_id,user_id,node_id)
            WHERE e.tenant_id=$1 AND e.user_id=$2 AND e.node_id=$3 AND q.limit_bytes IS NOT NULL
            AND {} >= q.limit_bytes)",
            crate::forwarding_quota::CYCLE_USED
        ))
        .bind(tenant_id)
        .bind(user_id)
        .bind(&write.node_id)
        .fetch_one(&mut **tx)
        .await?;
        if exhausted {
            return Err(StoreError::Conflict(
                "forwarding current cycle quota is exhausted; raise or remove the quota first"
                    .to_owned(),
            ));
        }
        ensure_static_port_available(tx, &write.node_id, write.protocol, write.listen_port).await?;
    }
    let count: i64 = row.try_get("rule_count")?;
    let max: i32 = row.try_get("max_rules")?;
    if count >= i64::from(max) {
        return Err(StoreError::Conflict(format!(
            "forwarding rule limit {max} has been reached"
        )));
    }
    let allow_private_targets: bool = row.try_get("allow_private_targets")?;
    if write
        .target_host
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .is_ok_and(|address| target_ip_is_blocked_by_policy(address, allow_private_targets))
    {
        return Err(StoreError::Forbidden(
            "forwarding target is blocked by the address policy".to_owned(),
        ));
    }
    Ok(())
}

/// Retire forbidden literal targets without changing allowed rules or their accounting identity.
/// Run on policy edits and desired-state reads so legacy rows cannot remain authorized forever.
async fn disable_blocked_rules(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    owner: Option<(&str, &str)>,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT r.id, r.target_host, e.allow_private_targets
           FROM forwarding_rules r JOIN forwarding_entitlements e
             ON e.tenant_id=r.tenant_id AND e.user_id=r.user_id AND e.node_id=r.node_id
          WHERE r.node_id=$1 AND r.deleted_at IS NULL AND r.enabled
            AND ($2::text IS NULL OR (r.tenant_id=$2 AND r.user_id=$3))
          ORDER BY r.id
          FOR UPDATE OF r",
    )
    .bind(node_id)
    .bind(owner.map(|value| value.0))
    .bind(owner.map(|value| value.1))
    .fetch_all(&mut **tx)
    .await?;
    let mut blocked = Vec::new();
    for row in rows {
        let host: String = row.try_get("target_host")?;
        let allow_private: bool = row.try_get("allow_private_targets")?;
        if host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|address| target_ip_is_blocked_by_policy(address, allow_private))
        {
            blocked.push(row.try_get::<i64, _>("id")?);
        }
    }
    if !blocked.is_empty() {
        sqlx::query("UPDATE forwarding_rules SET enabled=false, updated_at=now() WHERE id=ANY($1)")
            .bind(&blocked)
            .execute(&mut **tx)
            .await?;
        bump_node_generation(tx, node_id).await?;
    }
    Ok(())
}

async fn ensure_static_port_available(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    protocol: ForwardingProtocol,
    port: u16,
) -> Result<()> {
    let tcp = matches!(
        protocol,
        ForwardingProtocol::Tcp | ForwardingProtocol::TcpUdp
    );
    let udp = matches!(
        protocol,
        ForwardingProtocol::Udp | ForwardingProtocol::TcpUdp
    );
    let occupied: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM nodes n
             WHERE n.id = $1 AND (
                ($3 AND (
                    n.api_port = $2 OR
                    CASE WHEN n.wg_transport ->> 't' = 'fake_tcp'
                         THEN (n.wg_transport -> 'v' ->> 'port')::integer = $2
                         ELSE FALSE END
                )) OR ($4 AND n.wg_listen_port = $2)
             )
            UNION ALL
            SELECT 1 FROM ingresses i
             WHERE i.node_id = $1 AND (
                ($3 AND (
                    (i.transport_kind IS NOT NULL AND i.port = $2) OR
                    i.vless_encryption_port = $2 OR
                    (i.anytls_enabled AND i.anytls_port = $2) OR
                    i.mtproto_port = $2 OR
                    i.xhttp_download_v4_origin_port = $2 OR
                    i.xhttp_download_v6_origin_port = $2
                )) OR ($4 AND i.hy2_enabled AND
                    $2 BETWEEN COALESCE(i.hy2_hop_start, i.hy2_port)
                           AND COALESCE(i.hy2_hop_end, i.hy2_port))
             )
            UNION ALL
            SELECT 1 FROM steps s
             WHERE s.node_id = $1 AND $3 AND s.hop_in_port = $2
        )",
    )
    .bind(node_id)
    .bind(i32::from(port))
    .bind(tcp)
    .bind(udp)
    .fetch_one(&mut **tx)
    .await?;
    if occupied {
        return Err(StoreError::Conflict(format!(
            "port {port} is already used by a managed service on node {node_id}"
        )));
    }
    Ok(())
}

fn map_port_conflict<T>(result: std::result::Result<T, sqlx::Error>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(sqlx::Error::Database(error))
            if matches!(
                error.constraint(),
                Some("forwarding_rules_active_tcp_port" | "forwarding_rules_active_udp_port")
            ) =>
        {
            Err(StoreError::Conflict(
                "this forwarding listen port is already in use on the node".to_owned(),
            ))
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn desired_for_node(pool: &PgPool, node_id: &str) -> Result<ForwardingDesiredState> {
    let mut tx = pool.begin().await?;
    let desired = desired_for_node_tx(&mut tx, node_id).await?;
    crate::forwarding_quota::dispatched_tx(&mut tx, node_id, desired.generation).await?;
    tx.commit().await?;
    Ok(desired)
}

async fn desired_for_node_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<ForwardingDesiredState> {
    disable_blocked_rules(tx, node_id, None).await?;
    let generation = sqlx::query_scalar(
        "INSERT INTO forwarding_node_state (node_id) VALUES ($1)
         ON CONFLICT (node_id) DO UPDATE SET node_id = EXCLUDED.node_id
         RETURNING desired_generation",
    )
    .bind(node_id)
    .fetch_one(&mut **tx)
    .await?;
    let rows = sqlx::query(
        "SELECT r.id, r.protocol, r.listen_port, r.target_host, r.target_port, e.allow_private_targets
           FROM forwarding_rules r
           JOIN forwarding_entitlements e
             ON e.tenant_id = r.tenant_id AND e.user_id = r.user_id AND e.node_id = r.node_id
           JOIN users u ON u.tenant_id = r.tenant_id AND u.id = r.user_id
           JOIN nodes n ON n.id = r.node_id
           LEFT JOIN forwarding_node_quotas q ON q.tenant_id=r.tenant_id AND q.user_id=r.user_id AND q.node_id=r.node_id
          WHERE r.node_id = $1 AND r.deleted_at IS NULL AND r.enabled
            AND e.enabled AND NOT COALESCE(q.suspended,false) AND u.status = 'active' AND NOT u.service_suspended AND n.retired_at IS NULL
            AND r.listen_port BETWEEN e.listen_port_start AND e.listen_port_end
            AND (r.protocol NOT IN ('tcp', 'tcp_udp') OR e.tcp_allowed)
            AND (r.protocol NOT IN ('udp', 'tcp_udp') OR e.udp_allowed)
          ORDER BY r.id",
    )
    .bind(node_id)
    .fetch_all(&mut **tx)
    .await?;
    let rules = rows
        .into_iter()
        .map(|row| {
            Ok(ForwardingRuleDesired {
                id: row.try_get("id")?,
                protocol: parse_protocol(row.try_get("protocol")?)?,
                listen_port: integer_to_u16(row.try_get("listen_port")?)?,
                target_host: row.try_get("target_host")?,
                target_port: integer_to_u16(row.try_get("target_port")?)?,
                allow_private_targets: Some(row.try_get("allow_private_targets")?),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ForwardingDesiredState { generation, rules })
}

pub async fn record_reconcile(
    pool: &PgPool,
    node_id: &str,
    mut report: ForwardingReconcileReport,
) -> Result<ForwardingDesiredState> {
    report.applied_rule_ids.sort_unstable();
    report.applied_rule_ids.dedup();
    if report.applied_rule_ids.len() > 1_024 {
        return Err(StoreError::InvalidData(
            "forwarding reconcile report has too many rules".to_owned(),
        ));
    }
    if report
        .error
        .as_ref()
        .is_some_and(|error| error.len() > 4_096)
    {
        return Err(StoreError::InvalidData(
            "forwarding reconcile error is too long".to_owned(),
        ));
    }
    if let Some(results) = &report.rule_results {
        if results.len() > 1_024
            || results
                .iter()
                .any(|result| result.error.as_ref().is_some_and(|error| error.len() > 384))
        {
            return Err(StoreError::InvalidData(
                "forwarding rule results exceed their bounds".to_owned(),
            ));
        }
    }
    let mut tx = pool.begin().await?;
    let desired = desired_for_node_tx(&mut tx, node_id).await?;
    if desired.generation == report.generation {
        let wanted = desired
            .rules
            .iter()
            .map(|rule| rule.id)
            .collect::<BTreeSet<_>>();
        let applied = report
            .applied_rule_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        // Error reports describe observed listeners, including revoked rules whose removal
        // failed. This is node telemetry, never an authorization to install those rules.
        if report.error.is_none() && !applied.is_subset(&wanted) {
            return Err(StoreError::InvalidData(
                "forwarding reconcile report contains rules not desired on this node".to_owned(),
            ));
        }
        if report.error.is_none() && applied != wanted {
            return Err(StoreError::InvalidData(
                "successful forwarding reconcile report is incomplete".to_owned(),
            ));
        }
        let results = match report.rule_results {
            Some(mut results) => {
                let definitions = desired
                    .rules
                    .iter()
                    .map(|rule| (rule.id, rule))
                    .collect::<BTreeMap<_, _>>();
                let mut seen = BTreeSet::new();
                for result in &mut results {
                    // A legacy receipt can still prove a literal public target. A missing
                    // policy on a domain/private target cannot prove the new security boundary.
                    if result.rule.allow_private_targets.is_none()
                        && brocade_core::forwarding::target_host_is_public_ip(
                            &result.rule.target_host,
                        )
                    {
                        if let Some(expected) = definitions.get(&result.rule.id) {
                            result.rule.allow_private_targets = expected.allow_private_targets;
                        }
                    }
                    if !seen.insert(result.rule.id)
                        || definitions.get(&result.rule.id) != Some(&&result.rule)
                        || (result.applied
                            && (!applied.contains(&result.rule.id) || result.error.is_some()))
                        || (report.error.is_none() && (!result.applied || result.error.is_some()))
                    {
                        return Err(StoreError::InvalidData(
                            "forwarding rule result does not match the desired configuration"
                                .to_owned(),
                        ));
                    }
                }
                if seen != wanted {
                    return Err(StoreError::InvalidData(
                        "forwarding per-rule report is incomplete".to_owned(),
                    ));
                }
                Some(results)
            }
            // Legacy successful receipts still prove exact definitions at this generation.
            None if report.error.is_none() => Some(
                desired
                    .rules
                    .iter()
                    .cloned()
                    .map(|rule| ForwardingRuleReconcileResult {
                        rule,
                        applied: true,
                        error: None,
                    })
                    .collect(),
            ),
            None => None,
        };
        let results = results.map(|results| {
            results
                .into_iter()
                .map(|result| {
                    (
                        result.rule.id.to_string(),
                        serde_json::to_value(result).expect("forwarding result is serializable"),
                    )
                })
                .collect::<serde_json::Map<_, _>>()
        });
        sqlx::query(
            "UPDATE forwarding_node_state SET
                 applied_generation = $2,
                 reconcile_status = CASE WHEN $3::text IS NULL THEN 'converged' ELSE 'error' END,
                 applied_rule_ids = $4,
                 last_error = $3,
                 rule_results = $5,
                 reported_at = now()
              WHERE node_id = $1 AND desired_generation = $2",
        )
        .bind(node_id)
        .bind(report.generation)
        .bind(report.error.as_deref())
        .bind(json!(report.applied_rule_ids))
        .bind(results.map(serde_json::Value::Object))
        .execute(&mut *tx)
        .await?;
        crate::forwarding_quota::acknowledged_tx(
            &mut tx,
            node_id,
            report.generation,
            report.error.as_deref(),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(desired)
}

pub async fn prune_usage_report_receipts(pool: &PgPool, retain_days: u32) -> Result<u64> {
    prune_usage_report_receipts_hours(pool, retain_days.saturating_mul(24)).await
}

pub(crate) async fn prune_usage_report_receipts_hours(
    pool: &PgPool,
    retain_hours: u32,
) -> Result<u64> {
    if retain_hours == 0 {
        return Err(StoreError::InvalidData(
            "forwarding usage receipt retention must be at least one hour".to_owned(),
        ));
    }
    crate::retention::cleanup::prune_batch(pool, "forwarding_usage_report_receipts", retain_hours)
        .await
}

/// Fine windows are bounded; durable daily rollups and counter heads never expire here.
pub async fn prune_usage_samples(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let retain_hours = retain_days.saturating_mul(24);
    prune_usage_samples_hours(pool, retain_hours, retain_hours).await
}

pub(crate) async fn prune_usage_samples_hours(
    pool: &PgPool,
    sample_retain_hours: u32,
    window_retain_hours: u32,
) -> Result<u64> {
    if sample_retain_hours == 0 || window_retain_hours == 0 {
        return Err(StoreError::InvalidData(
            "invalid forwarding retention".to_owned(),
        ));
    }
    let samples = crate::retention::cleanup::prune_batch(
        pool,
        "forwarding_usage_samples",
        sample_retain_hours,
    )
    .await?;
    let windows = crate::retention::cleanup::prune_batch(
        pool,
        "forwarding_usage_windows",
        window_retain_hours,
    )
    .await?;
    Ok(samples + windows)
}

pub async fn record_usage(
    pool: &PgPool,
    node_id: &str,
    mut request: ForwardingUsageReportRequest,
) -> Result<ForwardingUsageReportResult> {
    if request.agent_instance_id.trim().is_empty() || request.agent_instance_id.len() > 128 {
        return Err(StoreError::InvalidData(
            "invalid forwarding usage agent instance id".to_owned(),
        ));
    }
    if request.xray_epoch.trim().is_empty() || request.xray_epoch.len() > 256 {
        return Err(StoreError::InvalidData(
            "invalid forwarding usage xray epoch".to_owned(),
        ));
    }
    if request.counters.len() > MAX_USAGE_COUNTERS {
        return Err(StoreError::InvalidData(
            "too many forwarding usage counters".to_owned(),
        ));
    }
    let sequence = i64::try_from(request.sequence)
        .map_err(|_| StoreError::InvalidData("forwarding usage sequence overflow".to_owned()))?;
    if sequence <= 0 || request.read_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "invalid forwarding sample time or sequence".to_owned(),
        ));
    }
    request.counters.sort_by_key(|counter| counter.rule_id);
    if request
        .counters
        .windows(2)
        .any(|pair| pair[0].rule_id == pair[1].rule_id)
    {
        return Err(StoreError::InvalidData(
            "duplicate forwarding counter".to_owned(),
        ));
    }
    let payload_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&request)?));
    let mut tx = pool.begin().await?;
    // Serialize first heads as well as existing ones; an old delayed report must never lower a
    // head and make the next sample charge the same bytes again.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 1179603505))")
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    let clock_valid: bool =
        sqlx::query_scalar("SELECT $1::bigint <= extract(epoch FROM now())::bigint+600")
            .bind(request.read_at_unix_secs)
            .fetch_one(&mut *tx)
            .await?;
    if !clock_valid {
        return Err(StoreError::InvalidData(
            "forwarding sample time is in the future".to_owned(),
        ));
    }
    if let Some(row) = sqlx::query(
        "SELECT result, payload_sha256 FROM forwarding_usage_report_receipts
          WHERE node_id = $1 AND agent_instance_id = $2 AND sequence = $3",
    )
    .bind(node_id)
    .bind(&request.agent_instance_id)
    .bind(sequence)
    .fetch_optional(&mut *tx)
    .await?
    {
        if row
            .try_get::<Option<String>, _>("payload_sha256")?
            .is_some_and(|hash| hash != payload_sha256)
        {
            return Err(StoreError::Conflict(
                "forwarding receipt payload changed".to_owned(),
            ));
        }
        let mut result: ForwardingUsageReportResult =
            serde_json::from_value(row.try_get("result")?)?;
        result.duplicate = true;
        tx.commit().await?;
        return Ok(result);
    }

    let mut accepted = 0_u64;
    let mut inserted = 0_u64;
    let mut skipped = 0_u64;
    for counter in request.counters {
        let owner = sqlx::query(
            "SELECT r.tenant_id, r.user_id,
                    (r.node_id = $2 AND r.enabled AND r.deleted_at IS NULL
                     AND EXISTS(SELECT 1 FROM forwarding_entitlements e
                       WHERE e.tenant_id=r.tenant_id AND e.user_id=r.user_id AND e.node_id=r.node_id AND e.enabled)
                     AND EXISTS(SELECT 1 FROM users u WHERE u.tenant_id=r.tenant_id AND u.id=r.user_id AND u.status='active')) AS active
               FROM forwarding_rules r
               LEFT JOIN forwarding_rule_nodes a ON a.rule_id = r.id AND a.node_id = $2
              WHERE r.id = $1 AND (r.node_id = $2 OR a.node_id IS NOT NULL)",
        )
        .bind(counter.rule_id)
        .bind(node_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(owner) = owner else {
            skipped += 1;
            continue;
        };
        let uplink = i64::try_from(counter.uplink_bytes).map_err(|_| {
            StoreError::InvalidData("forwarding uplink counter overflow".to_owned())
        })?;
        let downlink = i64::try_from(counter.downlink_bytes).map_err(|_| {
            StoreError::InvalidData("forwarding downlink counter overflow".to_owned())
        })?;
        let head = sqlx::query(
            "SELECT agent_instance_id, sequence, has_gap, xray_epoch, uplink_bytes, downlink_bytes,
                    extract(epoch FROM read_at)::bigint AS read_at
               FROM forwarding_usage_counter_heads
              WHERE node_id = $1 AND rule_id = $2 FOR UPDATE",
        )
        .bind(node_id)
        .bind(counter.rule_id)
        .fetch_optional(&mut *tx)
        .await?;
        let (start, delta_up, delta_down, has_gap, advance_head) = if let Some(head) = head {
            let previous_at: i64 = head.try_get("read_at")?;
            if request.read_at_unix_secs < previous_at
                || (head.try_get::<String, _>("agent_instance_id")? == request.agent_instance_id
                    && head
                        .try_get::<Option<i64>, _>("sequence")?
                        .is_some_and(|previous| sequence <= previous))
            {
                skipped += 1;
                continue;
            }
            let old_up: i64 = head.try_get("uplink_bytes")?;
            let old_down: i64 = head.try_get("downlink_bytes")?;
            // A durable counter identity survives Agent and controlled Xray restarts. The
            // reporting process identity only identifies receipts, not the lifetime of bytes.
            let same_epoch = head.try_get::<String, _>("xray_epoch")? == request.xray_epoch;
            let continuous = same_epoch && uplink >= old_up && downlink >= old_down;
            if request.read_at_unix_secs == previous_at
                && (!continuous || (uplink == old_up && downlink == old_down))
            {
                skipped += 1;
                continue;
            }
            if continuous {
                let uncertain: bool = head.try_get("has_gap")?;
                (
                    (request.read_at_unix_secs > previous_at && !uncertain).then_some(previous_at),
                    uplink - old_up,
                    downlink - old_down,
                    uncertain,
                    true,
                )
            } else if same_epoch {
                // A supposedly continuous durable counter went backwards. Its recovered prefix
                // was already charged; keep the high-water mark instead of charging it again.
                (None, 0, 0, true, false)
            } else {
                (None, uplink, downlink, true, true)
            }
        } else {
            // Tags contain a globally unique rule ID. Its first cumulative reading belongs to
            // this rule, including traffic already moved before the first report.
            // No earlier reading means no measured duration. Charge known bytes at observation
            // time, but never invent a one-second chart point or continuous coverage.
            (None, uplink, downlink, false, true)
        };
        let tenant: String = owner.try_get("tenant_id")?;
        let user: String = owner.try_get("user_id")?;
        if advance_head {
            sqlx::query(
                "INSERT INTO forwarding_usage_counter_heads (
                 node_id, rule_id, agent_instance_id, xray_epoch,
                 uplink_bytes, downlink_bytes, read_at, sequence, has_gap
             ) VALUES ($1,$2,$3,$4,$5,$6,to_timestamp($7),$8,false)
             ON CONFLICT (node_id, rule_id) DO UPDATE SET
                 agent_instance_id = EXCLUDED.agent_instance_id,
                 xray_epoch = EXCLUDED.xray_epoch,
                 uplink_bytes = EXCLUDED.uplink_bytes,
                 downlink_bytes = EXCLUDED.downlink_bytes,
                 read_at = EXCLUDED.read_at, sequence = EXCLUDED.sequence, has_gap=false",
            )
            .bind(node_id)
            .bind(counter.rule_id)
            .bind(&request.agent_instance_id)
            .bind(&request.xray_epoch)
            .bind(uplink)
            .bind(downlink)
            .bind(request.read_at_unix_secs as f64)
            .bind(sequence)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query("UPDATE forwarding_usage_counter_heads SET has_gap=true WHERE node_id=$1 AND rule_id=$2")
                .bind(node_id).bind(counter.rule_id).execute(&mut *tx).await?;
        }
        if delta_up > 0 || delta_down > 0 || has_gap {
            sqlx::query(
                "INSERT INTO forwarding_usage_samples (
                     node_id, rule_id, tenant_id, user_id, read_at, uplink_bytes, downlink_bytes,
                     window_start, has_gap
                 ) VALUES ($1,$2,$3,$4,to_timestamp($5),$6,$7,to_timestamp($8),$9)",
            )
            .bind(node_id)
            .bind(counter.rule_id)
            .bind(&tenant)
            .bind(&user)
            .bind(request.read_at_unix_secs as f64)
            .bind(delta_up)
            .bind(delta_down)
            .bind(start.map(|value| value as f64))
            .bind(has_gap)
            .execute(&mut *tx)
            .await?;
            inserted += 1;
            sqlx::query("INSERT INTO forwarding_usage_rollups
                (period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap)
                VALUES (date_trunc('day',to_timestamp($1) AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong',$2,$3,$4,$5,$6,$7,$8)
                ON CONFLICT (period_start,node_id,tenant_id,user_id,rule_id) DO UPDATE SET
                  uplink_bytes=forwarding_usage_rollups.uplink_bytes+EXCLUDED.uplink_bytes,
                  downlink_bytes=forwarding_usage_rollups.downlink_bytes+EXCLUDED.downlink_bytes,
                  has_gap=forwarding_usage_rollups.has_gap OR EXCLUDED.has_gap")
                .bind(start.unwrap_or(request.read_at_unix_secs) as f64).bind(node_id).bind(&tenant).bind(&user).bind(counter.rule_id)
                .bind(delta_up).bind(delta_down).bind(has_gap).execute(&mut *tx).await?;
        }
        if let Some(start) = start
            .filter(|_| owner.get::<bool, _>("active") || delta_up > 0 || delta_down > 0 || has_gap)
        {
            sqlx::query(
                "INSERT INTO forwarding_usage_windows
                (node_id,tenant_id,window_start,window_end,uplink_bytes,downlink_bytes,has_gap)
                VALUES ($1,$2,to_timestamp($3),to_timestamp($4),$5,$6,$7)
                ON CONFLICT (node_id,tenant_id,window_start,window_end) DO UPDATE SET
                  uplink_bytes=forwarding_usage_windows.uplink_bytes+EXCLUDED.uplink_bytes,
                  downlink_bytes=forwarding_usage_windows.downlink_bytes+EXCLUDED.downlink_bytes,
                  has_gap=forwarding_usage_windows.has_gap OR EXCLUDED.has_gap",
            )
            .bind(node_id)
            .bind(&tenant)
            .bind(start as f64)
            .bind(request.read_at_unix_secs as f64)
            .bind(delta_up)
            .bind(delta_down)
            .bind(has_gap)
            .execute(&mut *tx)
            .await?;
        }
        if advance_head {
            accepted += 1;
        } else {
            skipped += 1;
        }
    }
    let result = ForwardingUsageReportResult {
        node_id: node_id.to_owned(),
        agent_instance_id: request.agent_instance_id,
        sequence: request.sequence,
        duplicate: false,
        accepted_counters: accepted,
        inserted_samples: inserted,
        skipped_counters: skipped,
    };
    sqlx::query(
        "INSERT INTO forwarding_usage_report_receipts (
             node_id, agent_instance_id, sequence, result, payload_sha256
         ) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(node_id)
    .bind(&result.agent_instance_id)
    .bind(sequence)
    .bind(serde_json::to_value(&result)?)
    .bind(payload_sha256)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn remember_rule_node(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rule_id: i64,
    node_id: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO forwarding_rule_nodes (rule_id,node_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(rule_id)
    .bind(node_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn rule_view_from_row(row: sqlx::postgres::PgRow) -> Result<ForwardingRuleView> {
    Ok(ForwardingRuleView {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        user_id: row.try_get("user_id")?,
        node_id: row.try_get("node_id")?,
        name: row.try_get("name")?,
        protocol: parse_protocol(row.try_get("protocol")?)?,
        listen_port: integer_to_u16(row.try_get("listen_port")?)?,
        target_host: row.try_get("target_host")?,
        target_port: integer_to_u16(row.try_get("target_port")?)?,
        enabled: row.try_get("enabled")?,
        runtime_status: row.try_get("runtime_status")?,
        runtime_error: row.try_get("runtime_error")?,
        uplink_bytes: i64_to_u64(row.try_get("uplink_bytes")?)?,
        downlink_bytes: i64_to_u64(row.try_get("downlink_bytes")?)?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn integer_to_u16(value: i32) -> Result<u16> {
    u16::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("integer {value} does not fit u16")))
}

fn i64_to_u16(value: i64) -> Result<u16> {
    u16::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("integer {value} does not fit u16")))
}

fn i64_to_u64(value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("negative byte counter {value}")))
}

#[cfg(test)]
mod tests {
    use super::{address_is_private, validate_entitlement, ForwardingEntitlementWrite};

    #[test]
    fn tcp_ping_uses_rule_write_permissions_not_read_permissions() {
        use crate::{AdminContext, AdminRole};
        for role in [AdminRole::Readonly, AdminRole::User] {
            let actor = AdminContext::new("viewer", role, Some("t".to_owned()));
            assert!(super::require_rule_write_access(&actor, "t", "alice").is_err());
        }
        let editor = AdminContext::new("editor", AdminRole::Editor, Some("t".to_owned()));
        assert!(super::require_rule_write_access(&editor, "t", "alice").is_ok());
        assert!(super::require_rule_write_access(&editor, "other", "alice").is_err());
    }

    #[test]
    fn special_targets_are_private() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "224.0.0.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "2001:db8::1",
        ] {
            assert!(address_is_private(address.parse().unwrap()), "{address}");
        }
        assert!(!address_is_private("1.1.1.1".parse().unwrap()));
        assert!(!address_is_private("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn entitlement_defaults_allow_tcp_and_udp() {
        let value: ForwardingEntitlementWrite =
            serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(value.tcp_allowed);
        assert!(value.udp_allowed);
        assert!(!value.allow_private_targets);
        assert_eq!(value.listen_port_start, 30_000);
        assert_eq!(value.listen_port_end, 59_999);
        assert_eq!(value.max_rules, 100);
    }

    #[test]
    fn entitlement_can_explicitly_allow_private_targets() {
        let value = ForwardingEntitlementWrite {
            enabled: true,
            tcp_allowed: true,
            udp_allowed: true,
            listen_port_start: 40_000,
            listen_port_end: 49_999,
            max_rules: 16,
            allow_private_targets: true,
        };
        assert!(validate_entitlement(&value).is_ok());
    }
}
