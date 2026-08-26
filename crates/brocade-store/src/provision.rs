use std::{collections::HashSet, net::Ipv4Addr};

use brocade_core::model::{Dns, DomainStrategy};
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::input::{
    dns_kind, ensure_nonzero_port, optional_ipv6_text, required_text, validate_dns,
};
use crate::{
    credentials::{
        enrollment_token_display_prefix, enrollment_token_hash, generate_enrollment_token,
        generate_node_token, generate_wireguard_keypair, node_token_display_prefix,
        node_token_hash,
    },
    AdminContext, IssuedNodeToken, Result, StoreError,
};

const MIN_ENROLLMENT_TTL_SECONDS: u32 = 60;
const MAX_ENROLLMENT_TTL_SECONDS: u32 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionNodeRequest {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    #[serde(default)]
    pub public_ipv4: Option<String>,
    #[serde(default)]
    pub public_ipv6: Option<String>,
    #[serde(default)]
    pub public_ipv4_nat: bool,
    #[serde(default)]
    pub public_ipv6_nat: bool,
    pub wg_listen_port: u16,
    #[serde(default)]
    pub api_port: Option<u16>,
    #[serde(default = "default_backbone")]
    pub overlay: bool,
    #[serde(default)]
    pub egress_allowed: bool,
    pub dns: Dns,
    /// Absent means the default, which reproduces what the artifact layer used to
    /// hard-code — an existing caller that never heard of this field keeps its behaviour.
    #[serde(default)]
    pub domain_strategy: DomainStrategy,
    #[serde(default)]
    pub note: Option<String>,
    /// Which certificate group this machine draws its TLS / Hysteria 2 certificate from.
    ///
    /// Absent means the default group, which always exists — so a machine always has one, and
    /// nothing downstream has to handle "no group". Naming one here rather than later is
    /// deliberate: the group decides the machine's SNI, and changing it afterwards invalidates
    /// every subscription already handed out for that machine.
    #[serde(default)]
    pub cert_label_id: Option<String>,
    #[serde(default)]
    pub enrollment_ttl_seconds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionNodeResult {
    pub revision_id: u64,
    pub node: ProvisionedNode,
    pub enrollment: ProvisionedNodeEnrollment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionedNode {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub public_ipv4: Option<String>,
    pub public_ipv6: Option<String>,
    pub public_ipv4_nat: bool,
    pub public_ipv6_nat: bool,
    pub overlay_addr: Ipv4Addr,
    pub wg_public_key: String,
    pub wg_listen_port: u16,
    pub api_port: Option<u16>,
    pub overlay: bool,
    pub egress_allowed: bool,
    pub dns: Dns,
    pub domain_strategy: DomainStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionedNodeEnrollment {
    pub token: String,
    pub token_prefix: String,
    // No TTL means NULL = never expires; the expiry semantics are in the note on
    // insert_enrollment
    pub expires_at: Option<String>,
}

pub async fn provision_node(
    pool: &PgPool,
    actor: &AdminContext,
    request: ProvisionNodeRequest,
) -> Result<ProvisionNodeResult> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can provision nodes".to_owned(),
        ));
    }

    let input = NormalizedProvisionInput::from_request(request)?;
    let mut tx = pool.begin().await?;

    let state = sqlx::query(
        "SELECT overlay_cidr::text AS overlay_cidr
         FROM control_state
         WHERE id = TRUE
         FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;
    let overlay_cidr = state
        .try_get::<String, _>("overlay_cidr")?
        .parse::<Ipv4Net>()?;
    ensure_tenant_exists(&mut tx, &input.tenant_id).await?;
    ensure_node_missing(&mut tx, &input.id).await?;
    let overlay_addr = allocate_overlay_addr(&mut tx, overlay_cidr).await?;
    let wireguard = generate_wireguard_keypair()?;

    let revision_note = input
        .note
        .clone()
        .unwrap_or_else(|| format!("provision node {}", input.id));
    let revision_id =
        crate::console::insert_revision(&mut tx, actor.operator_id(), &revision_note).await?;

    let dns_kind = dns_kind(&input.dns);
    let dns_servers = dns_servers_json(&input.dns)?;
    sqlx::query(
        "INSERT INTO nodes (
            id, tenant_id, name, public_ipv4, public_ipv6, overlay_addr,
            wg_private_key, wg_public_key, wg_listen_port,
            api_port, overlay, egress_allowed,
            dns_kind, dns_servers, domain_strategy, public_ipv4_nat, public_ipv6_nat, created_revision
         ) VALUES (
            $1, $2, $3, $4, $5, $6::inet,
            $7, $8, $9,
            $10, $11, $12,
            $13, $14, $15, $16, $17, $18
         )",
    )
    .bind(&input.id)
    .bind(&input.tenant_id)
    .bind(&input.name)
    .bind(&input.public_ipv4)
    .bind(&input.public_ipv6)
    .bind(overlay_addr.to_string())
    .bind(&wireguard.private_key)
    .bind(&wireguard.public_key)
    .bind(i32::from(input.wg_listen_port))
    .bind(input.api_port.map(i32::from))
    .bind(input.overlay)
    .bind(input.egress_allowed)
    .bind(dns_kind)
    .bind(&dns_servers)
    .bind(domain_strategy_column(input.domain_strategy)?)
    .bind(input.public_ipv4_nat)
    .bind(input.public_ipv6_nat)
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE control_state SET current_revision = $1 WHERE id = TRUE")
        .bind(u64_to_i64(revision_id, "revision_id")?)
        .execute(&mut *tx)
        .await?;
    // Before the snapshot is stored: the snapshot reads `certificate_name` through this row, and
    // a machine provisioned into a group whose certificate is already serving should compile with
    // that name straight away rather than on the next revision.
    crate::cert::assign_node_label(&mut tx, &input.id, input.cert_label_id.as_deref()).await?;

    crate::materialize::store_current_snapshot_tx(&mut tx, revision_id).await?;

    let enrollment = insert_enrollment(
        &mut tx,
        &input.id,
        input.enrollment_ttl_seconds,
        actor.operator_id(),
    )
    .await?;

    tx.commit().await?;

    Ok(ProvisionNodeResult {
        revision_id,
        node: ProvisionedNode {
            id: input.id,
            tenant_id: input.tenant_id,
            name: input.name,
            public_ipv4: input.public_ipv4,
            public_ipv6: input.public_ipv6,
            public_ipv4_nat: input.public_ipv4_nat,
            public_ipv6_nat: input.public_ipv6_nat,
            overlay_addr,
            wg_public_key: wireguard.public_key,
            wg_listen_port: input.wg_listen_port,
            api_port: input.api_port,
            overlay: input.overlay,
            egress_allowed: input.egress_allowed,
            dns: input.dns,
            domain_strategy: input.domain_strategy,
        },
        enrollment,
    })
}

pub async fn redeem_node_enrollment(pool: &PgPool, token: &str) -> Result<IssuedNodeToken> {
    let token = token.trim();
    if token.is_empty() {
        return Err(StoreError::Unauthorized(
            "missing enrollment token".to_owned(),
        ));
    }

    let token_hash = enrollment_token_hash(token);
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT id, node_id
         FROM node_enrollments
         WHERE token_hash = $1
           AND used_at IS NULL
           AND (expires_at IS NULL OR expires_at > now())
         FOR UPDATE",
    )
    .bind(token_hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Err(StoreError::Unauthorized(
            "invalid or expired enrollment token".to_owned(),
        ));
    };

    let enrollment_id: i64 = row.try_get("id")?;
    let node_id: String = row.try_get("node_id")?;
    let issued = issue_node_token_in_tx(&mut tx, &node_id).await?;
    sqlx::query("UPDATE node_enrollments SET used_at = now() WHERE id = $1")
        .bind(enrollment_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(issued)
}

async fn ensure_tenant_exists(tx: &mut Transaction<'_, Postgres>, tenant_id: &str) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(StoreError::NotFound(format!("tenant {tenant_id}")))
    }
}

async fn ensure_node_missing(tx: &mut Transaction<'_, Postgres>, node_id: &str) -> Result<()> {
    let exists = sqlx::query("SELECT 1 FROM nodes WHERE id = $1")
        .bind(node_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    if exists {
        Err(StoreError::Unsupported(format!(
            "node {node_id} already exists"
        )))
    } else {
        Ok(())
    }
}

async fn allocate_overlay_addr(
    tx: &mut Transaction<'_, Postgres>,
    overlay_cidr: Ipv4Net,
) -> Result<Ipv4Addr> {
    let rows = sqlx::query("SELECT overlay_addr::text AS overlay_addr FROM nodes")
        .fetch_all(&mut **tx)
        .await?;
    let used = rows
        .iter()
        .map(|row| parse_ipv4(&row.try_get::<String, _>("overlay_addr")?))
        .collect::<Result<HashSet<_>>>()?;

    next_overlay_addr(overlay_cidr, &used).ok_or_else(|| {
        StoreError::Unsupported(format!(
            "overlay cidr {overlay_cidr} has no available addresses"
        ))
    })
}

fn next_overlay_addr(overlay_cidr: Ipv4Net, used: &HashSet<Ipv4Addr>) -> Option<Ipv4Addr> {
    let network = u32::from(overlay_cidr.network());
    let broadcast = u32::from(overlay_cidr.broadcast());
    let (first, last) = if overlay_cidr.prefix_len() >= 31 {
        (network, broadcast)
    } else {
        (network.saturating_add(1), broadcast.saturating_sub(1))
    };

    (first..=last)
        .map(Ipv4Addr::from)
        .find(|addr| overlay_cidr.contains(addr) && !used.contains(addr))
}

async fn insert_enrollment(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    ttl_seconds: Option<u32>,
    actor: &str,
) -> Result<ProvisionedNodeEnrollment> {
    let token = generate_enrollment_token()?;
    let token_hash = enrollment_token_hash(&token);
    let token_prefix = enrollment_token_display_prefix(&token);
    // With ttl None no expiry is written (NULL = never expires). An enrollment token is
    // single-use (used_at redeems it once), so "no expiry" does not turn it into a long-lived
    // credential; it merely stops forcing someone to paste the command onto a machine within
    // 15 minutes — copying it from the console into an SSH session very easily runs over.
    let row = sqlx::query(
        "INSERT INTO node_enrollments (
            node_id, token_hash, token_prefix, expires_at, created_by
         )
         VALUES ($1, $2, $3, CASE WHEN $4::bigint IS NULL THEN NULL ELSE now() + ($4::bigint * interval '1 second') END, $5)
         RETURNING token_prefix, expires_at::text AS expires_at",
    )
    .bind(node_id)
    .bind(token_hash)
    .bind(&token_prefix)
    .bind(ttl_seconds.map(i64::from))
    .bind(actor)
    .fetch_one(&mut **tx)
    .await?;

    Ok(ProvisionedNodeEnrollment {
        token,
        token_prefix: row.try_get("token_prefix")?,
        expires_at: row.try_get::<Option<String>, _>("expires_at")?,
    })
}

async fn issue_node_token_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<IssuedNodeToken> {
    let token = generate_node_token()?;
    let token_hash = node_token_hash(&token);
    let token_prefix = node_token_display_prefix(&token);
    let row = sqlx::query(
        // The token counts as used the moment it is redeemed (install.sh completed the
        // enrollment with it). Setting token_last_used_at advances step 4's probe from
        // "awaiting redemption" to "awaiting heartbeat"; without it the UI sits at waiting
        // until the agent's first authentication.
        "INSERT INTO node_agent_state (
            node_id, token_hash, token_prefix, token_created_at,
            token_last_used_at, token_revoked_at
         )
         VALUES ($1, $2, $3, now(), now(), NULL)
         ON CONFLICT (node_id) DO UPDATE SET
            token_hash = EXCLUDED.token_hash,
            token_prefix = EXCLUDED.token_prefix,
            token_created_at = EXCLUDED.token_created_at,
            token_last_used_at = EXCLUDED.token_last_used_at,
            token_revoked_at = NULL
         RETURNING node_id, token_prefix",
    )
    .bind(node_id)
    .bind(token_hash)
    .bind(token_prefix)
    .fetch_one(&mut **tx)
    .await?;

    Ok(IssuedNodeToken {
        node_id: row.try_get("node_id")?,
        token,
        token_prefix: row.try_get("token_prefix")?,
    })
}

#[derive(Debug)]
struct NormalizedProvisionInput {
    id: String,
    tenant_id: String,
    name: String,
    public_ipv4: Option<String>,
    public_ipv6: Option<String>,
    public_ipv4_nat: bool,
    public_ipv6_nat: bool,
    wg_listen_port: u16,
    api_port: Option<u16>,
    overlay: bool,
    egress_allowed: bool,
    dns: Dns,
    domain_strategy: DomainStrategy,
    note: Option<String>,
    cert_label_id: Option<String>,
    // None means never expires (the default); only Some is clamped to MIN..=MAX
    enrollment_ttl_seconds: Option<u32>,
}

impl NormalizedProvisionInput {
    fn from_request(request: ProvisionNodeRequest) -> Result<Self> {
        let id = crate::console::required_slug(request.id, "node id")?;
        let tenant_id = required_text(request.tenant_id, "tenant_id")?;
        let name = required_text(request.name, "node name")?;
        let public_ipv4 = optional_text(request.public_ipv4);
        let public_ipv6 = optional_ipv6_text(request.public_ipv6.as_deref(), "public_ipv6")?;
        ensure_nonzero_port(request.wg_listen_port, "wg_listen_port")?;
        if let Some(api_port) = request.api_port {
            ensure_nonzero_port(api_port, "api_port")?;
        }
        validate_dns(&request.dns)?;
        let enrollment_ttl_seconds = request.enrollment_ttl_seconds;
        if let Some(ttl) = enrollment_ttl_seconds {
            if !(MIN_ENROLLMENT_TTL_SECONDS..=MAX_ENROLLMENT_TTL_SECONDS).contains(&ttl) {
                return Err(StoreError::InvalidData(format!(
                    "enrollment_ttl_seconds must be between {MIN_ENROLLMENT_TTL_SECONDS} and {MAX_ENROLLMENT_TTL_SECONDS}"
                )));
            }
        }

        Ok(Self {
            id,
            tenant_id,
            name,
            public_ipv4,
            public_ipv6,
            public_ipv4_nat: request.public_ipv4_nat,
            public_ipv6_nat: request.public_ipv6_nat,
            wg_listen_port: request.wg_listen_port,
            api_port: request.api_port,
            overlay: request.overlay,
            egress_allowed: request.egress_allowed,
            dns: request.dns,
            domain_strategy: request.domain_strategy,
            note: optional_text(request.note),
            cert_label_id: request
                .cert_label_id
                .map(|id| id.trim().to_owned())
                .filter(|id| !id.is_empty()),
            enrollment_ttl_seconds,
        })
    }
}

fn default_backbone() -> bool {
    true
}

fn optional_text(value: Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn dns_servers_json(dns: &Dns) -> Result<serde_json::Value> {
    match dns {
        Dns::System => Ok(serde_json::json!([])),
        Dns::Servers(servers) => Ok(serde_json::to_value(servers)?),
    }
}

/// The column holds `DomainStrategy`'s serde spelling, which is also what the CHECK
/// constraint lists. Going through serde rather than a match keeps the two in step: a new
/// variant needs the constraint updated and nothing else.
fn domain_strategy_column(strategy: DomainStrategy) -> Result<String> {
    match serde_json::to_value(strategy)? {
        serde_json::Value::String(value) => Ok(value),
        other => Err(StoreError::InvalidData(format!(
            "domain strategy did not serialize to a string: {other}"
        ))),
    }
}

fn parse_ipv4(value: &str) -> Result<Ipv4Addr> {
    let value = value.split_once('/').map_or(value, |(addr, _)| addr);
    value
        .parse()
        .map_err(|error| StoreError::InvalidData(format!("invalid IPv4 address {value}: {error}")))
}

fn u64_to_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} out of range")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_overlay_addr_skips_network_broadcast_and_used_addresses() {
        let cidr = "10.66.0.0/29".parse::<Ipv4Net>().unwrap();
        let used = [Ipv4Addr::new(10, 66, 0, 1), Ipv4Addr::new(10, 66, 0, 2)]
            .into_iter()
            .collect::<HashSet<_>>();

        assert_eq!(
            next_overlay_addr(cidr, &used),
            Some(Ipv4Addr::new(10, 66, 0, 3))
        );
    }

    #[test]
    fn provision_input_rejects_empty_required_values_and_zero_ports() {
        let error = NormalizedProvisionInput::from_request(ProvisionNodeRequest {
            id: " ".to_owned(),
            tenant_id: "platform.acme".to_owned(),
            name: "Node".to_owned(),
            public_ipv4: None,
            public_ipv6: None,
            public_ipv4_nat: false,
            public_ipv6_nat: false,
            wg_listen_port: 51820,
            api_port: None,
            overlay: true,
            egress_allowed: false,
            dns: Dns::System,
            domain_strategy: DomainStrategy::default(),
            note: None,
            cert_label_id: None,
            enrollment_ttl_seconds: None,
        })
        .unwrap_err();
        assert!(matches!(error, StoreError::InvalidData(_)));

        let error = NormalizedProvisionInput::from_request(ProvisionNodeRequest {
            id: "n1".to_owned(),
            tenant_id: "platform.acme".to_owned(),
            name: "Node".to_owned(),
            public_ipv4: None,
            public_ipv6: None,
            public_ipv4_nat: false,
            public_ipv6_nat: false,
            wg_listen_port: 0,
            api_port: None,
            overlay: true,
            egress_allowed: false,
            dns: Dns::System,
            domain_strategy: DomainStrategy::default(),
            note: None,
            cert_label_id: None,
            enrollment_ttl_seconds: None,
        })
        .unwrap_err();
        assert!(matches!(error, StoreError::InvalidData(_)));
    }
}
