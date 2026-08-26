//! Public Clash subscriptions. Nothing in this module writes: each call reads the current
//! materialized model, projects the user, compiles YAML, and accounts the current calendar month.

use std::collections::BTreeSet;

use brocade_core::{
    artifacts::subscription, compile::compile, format::yaml, model::IpFamily,
    physical::user::project_user,
};
use sqlx::{PgPool, Row};

use super::*;
use crate::{AdminContext, Result, StoreError};

const PUBLIC_NOT_FOUND: &str = "subscription not found";

/// Resolve an active user by bearer UUID and compile their current subscription. Inactive users
/// are absent from the current snapshot, so absent and inactive credentials have one answer.
pub async fn clash_subscription_by_uuid(
    pool: &PgPool,
    uuid: &str,
) -> Result<DynamicClashSubscription> {
    clash_subscription_by_uuid_for_family(pool, uuid, None).await
}

/// The family-specific public URLs are alternate views of the same live projection. Keeping the
/// filter here ensures VLESS artifacts and Clash subscriptions both use `UserPlan::retain_family`
/// rather than growing subtly different notions of an IPv4/IPv6-capable entry.
pub async fn clash_subscription_by_uuid_for_family(
    pool: &PgPool,
    uuid: &str,
    family: Option<IpFamily>,
) -> Result<DynamicClashSubscription> {
    let snapshot = crate::materialize::load_current_snapshot(pool).await?;
    let user = snapshot
        .users
        .iter()
        .find(|user| user.uuid == uuid)
        .ok_or_else(public_not_found)?;
    build_dynamic_clash(pool, &snapshot, &user.tenant, &user.id, family, true).await
}

/// The administrator-side lookup uses the already scoped snapshot. A tenant administrator can
/// therefore obtain URLs only for users they can see, and readonly reviewers are still refused
/// by the HTTP artifact permission before this function is called.
pub async fn clash_subscription_for_user(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<DynamicClashSubscription> {
    let snapshot = load_scoped_snapshot(pool, actor, None).await?;
    if !snapshot
        .users
        .iter()
        .any(|user| user.tenant == tenant_id && user.id == user_id)
    {
        return Err(StoreError::NotFound(format!("user {tenant_id}/{user_id}")));
    }
    build_dynamic_clash(pool, &snapshot, tenant_id, user_id, None, false).await
}

async fn build_dynamic_clash(
    pool: &PgPool,
    snapshot: &brocade_core::model::ModelSnapshot,
    tenant_id: &str,
    user_id: &str,
    family: Option<IpFamily>,
    hide_reason: bool,
) -> Result<DynamicClashSubscription> {
    let output = compile(snapshot);
    let mut plan = output.project_user(tenant_id, user_id).map_err(|blocked| {
        StoreError::InvalidData(format!(
            "cannot project user {tenant_id}/{user_id}: {blocked:?}"
        ))
    })?;
    if plan.entries.is_empty() {
        return Err(if hide_reason {
            public_not_found()
        } else {
            StoreError::NotFound(format!(
                "user {tenant_id}/{user_id} has no effective subscription"
            ))
        });
    }
    if let Some(family) = family {
        plan.retain_family(family);
    }

    // Only apps that actually project at least one subscription entry participate in the
    // combined quota. A grant whose ingress has no usable public endpoint does not silently make
    // an otherwise limited subscription appear unlimited.
    let app_ids = output
        .unpublishable_view()
        .apps
        .iter()
        .filter_map(|app| {
            (!project_user(std::slice::from_ref(app), tenant_id, user_id)
                .entries
                .is_empty())
            .then(|| app.app_id.clone())
            .flatten()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if app_ids.is_empty() {
        return Err(if hide_reason {
            public_not_found()
        } else {
            StoreError::NotFound(format!(
                "user {tenant_id}/{user_id} has no effective subscription"
            ))
        });
    }

    let uuid = plan.uuid.clone();
    let content = yaml::clash_subscription(&subscription::build(&plan));
    let usage = subscription_usage(pool, tenant_id, user_id, &app_ids).await?;
    Ok(DynamicClashSubscription {
        tenant_id: tenant_id.to_owned(),
        user_id: user_id.to_owned(),
        uuid,
        revision: snapshot.revision,
        content,
        usage,
    })
}

async fn subscription_usage(
    pool: &PgPool,
    tenant_id: &str,
    user_id: &str,
    app_ids: &[String],
) -> Result<ClashSubscriptionUsage> {
    let row = sqlx::query(
        "WITH bounds AS (
             SELECT date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                        AT TIME ZONE 'Asia/Hong_Kong' AS month_start,
                    (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                         + INTERVAL '1 month') AT TIME ZONE 'Asia/Hong_Kong' AS month_end
         ), used AS (
             SELECT coalesce(sum(s.uplink_bytes), 0)::bigint AS upload_bytes,
                    coalesce(sum(s.downlink_bytes), 0)::bigint AS download_bytes,
                    coalesce(bool_or(s.has_gap), false) AS has_gap
             FROM usage_samples s, bounds b
             WHERE s.tenant_id = $1
               AND s.user_id = $2
               AND s.app_id = ANY($3::text[])
               AND s.window_start >= b.month_start
               AND s.window_start < b.month_end
         ), limits AS (
             SELECT count(*)::bigint AS limited_count,
                    coalesce(sum(q.limit_bytes), 0)::bigint AS total_bytes
             FROM user_app_quotas q
             WHERE q.tenant_id = $1
               AND q.user_id = $2
               AND q.app_id = ANY($3::text[])
         )
         SELECT u.upload_bytes, u.download_bytes, u.has_gap,
                CASE WHEN l.limited_count = cardinality($3::text[])
                     THEN l.total_bytes ELSE NULL END AS total_bytes,
                to_char(b.month_end AT TIME ZONE 'Asia/Hong_Kong',
                        'YYYY-MM-DD\"T\"HH24:MI:SS') || '+08:00' AS reset_at
         FROM used u, limits l, bounds b",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(app_ids)
    .fetch_one(pool)
    .await?;

    let upload_bytes = nonnegative_bytes("upload_bytes", row.try_get("upload_bytes")?)?;
    let download_bytes = nonnegative_bytes("download_bytes", row.try_get("download_bytes")?)?;
    let total_bytes = row
        .try_get::<Option<i64>, _>("total_bytes")?
        .map(|value| nonnegative_bytes("total_bytes", value))
        .transpose()?;
    let remaining_bytes =
        total_bytes.map(|total| total.saturating_sub(upload_bytes.saturating_add(download_bytes)));

    Ok(ClashSubscriptionUsage {
        upload_bytes,
        download_bytes,
        total_bytes,
        remaining_bytes,
        reset_at: row.try_get("reset_at")?,
        has_gap: row.try_get("has_gap")?,
    })
}

fn nonnegative_bytes(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        StoreError::InvalidData(format!(
            "subscription usage {field} cannot be negative: {value}"
        ))
    })
}

fn public_not_found() -> StoreError {
    StoreError::NotFound(PUBLIC_NOT_FOUND.to_owned())
}
