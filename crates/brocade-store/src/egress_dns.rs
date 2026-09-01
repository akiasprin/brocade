//! Machine-owned Xray DNS policies.
//!
//! The resolver is stored once per node and selector. A chain egress rule may activate that
//! selector, but the policy remains global to the machine's Xray instance and is never copied
//! into a chain.

use std::net::IpAddr;

use brocade_core::model::{DestMatch, EgressDnsResolution, ModelSnapshot, NodeEgressDnsPolicy};
use sqlx::{Postgres, Row, Transaction};

use crate::console::u64_to_i64;
use crate::{AdminContext, Result, StoreError};

pub(crate) type StoredPolicy = (String, u32, DestMatch, EgressDnsResolution);

/// The selector identity used by the database and UI sharing semantics.
///
/// Lists are sets in routing rules. Sorting and deduplicating here makes `geosite:[a,b]` and
/// `geosite:[b,a]` one machine policy rather than two values which Xray cannot distinguish.
pub(crate) fn canonical_selector(dest_match: &DestMatch) -> Option<DestMatch> {
    dest_match.canonical_egress_dns_selector()
}

pub(crate) fn model_policies(policies: &[StoredPolicy]) -> Vec<NodeEgressDnsPolicy> {
    policies
        .iter()
        .map(
            |(node, position, selector, resolution)| NodeEgressDnsPolicy {
                node: node.clone(),
                position: *position,
                selector: selector.clone(),
                resolution: resolution.clone(),
            },
        )
        .collect()
}

/// Write the machine policy directly. Route-table writes deliberately do not call this: a chain
/// may activate a selector, but it does not own the resolver or its lifetime.
pub(crate) async fn set_policy_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    node_id: &str,
    selector: DestMatch,
    resolution: Option<EgressDnsResolution>,
) -> Result<bool> {
    let node_id = node_id.trim();
    if node_id.is_empty() {
        return Err(StoreError::InvalidData("node id 不能为空".to_owned()));
    }
    let selector = canonical_selector(&selector).ok_or_else(|| {
        StoreError::InvalidData(
            "机器 DNS 策略只支持域名后缀、域名关键词、域名正则和 Geosite".to_owned(),
        )
    })?;
    let selector_is_empty = match &selector {
        DestMatch::DomainSuffix(values)
        | DestMatch::DomainKeyword(values)
        | DestMatch::Geosite(values) => {
            values.is_empty() || values.iter().any(|value| value.trim().is_empty())
        }
        DestMatch::DomainRegex(value) => value.trim().is_empty(),
        _ => true,
    };
    if selector_is_empty {
        return Err(StoreError::InvalidData(
            "机器 DNS 策略的匹配条件不能为空".to_owned(),
        ));
    }
    // Lock the owner row so concurrent append/reorder operations on one machine serialize.
    let tenant_id = sqlx::query("SELECT tenant_id FROM nodes WHERE id = $1 FOR UPDATE")
        .bind(node_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?
        .try_get::<String, _>("tenant_id")?;
    actor.require_tenant_access(&tenant_id, "node egress DNS")?;

    let selector = serde_json::to_value(selector)?;
    match resolution {
        Some(resolution) => {
            if resolution.address.trim().parse::<IpAddr>().is_err() {
                return Err(StoreError::InvalidData(
                    "指定 DNS 的地址必须是 IPv4 或 IPv6 字面量，不能填写域名".to_owned(),
                ));
            }
            if resolution.port == 0 {
                return Err(StoreError::InvalidData(
                    "指定 DNS 的端口必须在 1–65535 之间".to_owned(),
                ));
            }
            Ok(sqlx::query(
                "INSERT INTO node_egress_dns (
                    node_id, position, selector, resolution, created_revision
                 ) VALUES (
                    $1,
                    COALESCE((SELECT MAX(position) + 1 FROM node_egress_dns WHERE node_id = $1), 0),
                    $2, $3, $4
                 )
                 ON CONFLICT (node_id, selector) DO UPDATE SET
                    resolution = EXCLUDED.resolution
                 WHERE node_egress_dns.resolution IS DISTINCT FROM EXCLUDED.resolution",
            )
            .bind(node_id)
            .bind(selector)
            .bind(serde_json::to_value(resolution)?)
            .bind(u64_to_i64(revision_id, "revision_id")?)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                > 0)
        }
        None => {
            let removed = sqlx::query_scalar::<_, i32>(
                "DELETE FROM node_egress_dns
                 WHERE node_id = $1 AND selector = $2
                 RETURNING position",
            )
            .bind(node_id)
            .bind(selector)
            .fetch_optional(&mut **tx)
            .await?;
            let Some(position) = removed else {
                return Ok(false);
            };
            sqlx::query(
                "UPDATE node_egress_dns SET position = position - 1
                 WHERE node_id = $1 AND position > $2",
            )
            .bind(node_id)
            .bind(position)
            .execute(&mut **tx)
            .await?;
            Ok(true)
        }
    }
}

/// Replace one machine's complete DNS priority list. Selectors, rather than row ids, are the
/// stable public identity; the deferred `(node_id, position)` constraint permits swaps inside a
/// transaction while still rejecting a duplicate final priority.
pub(crate) async fn reorder_policies_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    node_id: &str,
    selectors: Vec<DestMatch>,
) -> Result<bool> {
    let node_id = node_id.trim();
    if node_id.is_empty() {
        return Err(StoreError::InvalidData("node id 不能为空".to_owned()));
    }
    let tenant_id = sqlx::query("SELECT tenant_id FROM nodes WHERE id = $1 FOR UPDATE")
        .bind(node_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?
        .try_get::<String, _>("tenant_id")?;
    actor.require_tenant_access(&tenant_id, "node egress DNS")?;

    let mut requested = Vec::with_capacity(selectors.len());
    for selector in selectors {
        let selector = canonical_selector(&selector).ok_or_else(|| {
            StoreError::InvalidData(
                "机器 DNS 策略只支持域名后缀、域名关键词、域名正则和 Geosite".to_owned(),
            )
        })?;
        if requested.contains(&selector) {
            return Err(StoreError::InvalidData(
                "机器 DNS 策略排序中不能包含重复条件".to_owned(),
            ));
        }
        requested.push(selector);
    }

    let current = sqlx::query(
        "SELECT selector FROM node_egress_dns
         WHERE node_id = $1 ORDER BY position, selector FOR UPDATE",
    )
    .bind(node_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|row| serde_json::from_value::<DestMatch>(row.try_get("selector")?).map_err(Into::into))
    .collect::<Result<Vec<_>>>()?;
    if requested.len() != current.len()
        || requested.iter().any(|selector| !current.contains(selector))
    {
        return Err(StoreError::InvalidData(
            "机器 DNS 策略排序必须完整包含这台机器的当前全部策略，请刷新后重试".to_owned(),
        ));
    }
    if requested == current {
        return Ok(false);
    }

    for (position, selector) in requested.into_iter().enumerate() {
        sqlx::query(
            "UPDATE node_egress_dns SET position = $3
             WHERE node_id = $1 AND selector = $2",
        )
        .bind(node_id)
        .bind(serde_json::to_value(selector)?)
        .bind(
            i32::try_from(position).map_err(|_| {
                StoreError::InvalidData("机器 DNS 策略数量超出数据库范围".to_owned())
            })?,
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(true)
}

/// Return the canonical rows represented by a model snapshot during rollback.
///
/// Snapshots written before machine-owned DNS policies existed are deliberately unsupported. The
/// development deployment clears that history once instead of carrying two ownership models in
/// every future restore path.
pub(crate) fn policies_from_snapshot(snapshot: &ModelSnapshot) -> Vec<StoredPolicy> {
    snapshot
        .node_egress_dns
        .iter()
        .map(|policy| {
            (
                policy.node.clone(),
                policy.position,
                policy.selector.clone(),
                policy.resolution.clone(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_lists_are_one_set_regardless_of_order() {
        assert_eq!(
            canonical_selector(&DestMatch::Geosite(vec!["b".to_owned(), "a".to_owned()])),
            canonical_selector(&DestMatch::Geosite(vec!["a".to_owned(), "b".to_owned()]))
        );
    }
}
