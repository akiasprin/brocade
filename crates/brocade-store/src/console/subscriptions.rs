//! Serving client subscriptions. Every pull renders fresh content
//! from the last fully converged serving projection and accounts the current calendar month.
//! Committed-but-unpublished revisions are intentionally invisible, while an open/uncertain
//! release makes pulls temporarily unavailable instead of returning a configuration which may not
//! match the fleet. Administrator-only sharing links are separate operational state.

use std::collections::BTreeSet;

use brocade_core::{
    artifacts::subscription,
    format::{uri, yaml},
    model::IpFamily,
    physical::user::{project_user, SubscriptionFilter},
};
use sqlx::{PgPool, Row};

use super::*;
use crate::{
    credentials::generate_uuid_v4, input::required_text, AdminContext, Result, StoreError,
};

const PUBLIC_NOT_FOUND: &str = "subscription not found";

#[derive(Clone, Copy)]
enum DynamicClashTemplate {
    Standard,
    Haitun,
    Shadowrocket,
}

/// Resolve an active user by bearer UUID and compile their serving subscription. Inactive users
/// are absent from the serving snapshot, so absent and inactive credentials have one answer.
pub async fn clash_subscription_by_uuid(
    pool: &PgPool,
    uuid: &str,
) -> Result<DynamicClashSubscription> {
    clash_subscription_by_uuid_filtered(pool, uuid, SubscriptionFilter::default()).await
}

/// The family-specific public URLs are alternate views of the same serving projection. Keeping the
/// filter here ensures VLESS artifacts and Clash subscriptions both use `UserPlan::retain_family`
/// rather than growing subtly different notions of an IPv4/IPv6-capable entry.
pub async fn clash_subscription_by_uuid_for_family(
    pool: &PgPool,
    uuid: &str,
    family: Option<IpFamily>,
) -> Result<DynamicClashSubscription> {
    clash_subscription_by_uuid_filtered(
        pool,
        uuid,
        SubscriptionFilter {
            family,
            ..Default::default()
        },
    )
    .await
}

/// Resolve the public bearer and narrow the serving projection by address family and/or client
/// protocol. Both filters operate on the same compiled plan so a combined URL cannot drift from
/// the individual views.
pub async fn clash_subscription_by_uuid_filtered(
    pool: &PgPool,
    uuid: &str,
    filter: SubscriptionFilter,
) -> Result<DynamicClashSubscription> {
    subscription_by_uuid(pool, uuid, filter, DynamicClashTemplate::Standard).await
}

pub async fn shadowrocket_subscription_by_uuid_filtered(
    pool: &PgPool,
    uuid: &str,
    filter: SubscriptionFilter,
) -> Result<DynamicClashSubscription> {
    subscription_by_uuid(pool, uuid, filter, DynamicClashTemplate::Shadowrocket).await
}

async fn subscription_by_uuid(
    pool: &PgPool,
    uuid: &str,
    filter: SubscriptionFilter,
    template: DynamicClashTemplate,
) -> Result<DynamicClashSubscription> {
    let serving = crate::serving::load_subscription_serving_projection(pool).await?;
    let user = serving
        .snapshot
        .users
        .iter()
        .find(|user| user.uuid == uuid)
        .ok_or_else(public_not_found)?;
    let tenant_id = user.tenant.clone();
    let user_id = user.id.clone();
    serving.ensure_available()?;
    build_dynamic_clash(
        pool,
        &serving.snapshot,
        &tenant_id,
        &user_id,
        filter,
        template,
        true,
    )
    .await
}

/// Resolve a separately revocable Haitun bearer. The lookup deliberately happens before model
/// projection: revoked, unknown, disabled and currently unusable users all have the same public
/// answer and disclose no account state.
pub async fn clash_subscription_by_haitun_token_for_family(
    pool: &PgPool,
    token: &str,
    family: Option<IpFamily>,
) -> Result<DynamicClashSubscription> {
    clash_subscription_by_haitun_token_filtered(
        pool,
        token,
        SubscriptionFilter {
            family,
            ..Default::default()
        },
    )
    .await
}

pub async fn clash_subscription_by_haitun_token_filtered(
    pool: &PgPool,
    token: &str,
    filter: SubscriptionFilter,
) -> Result<DynamicClashSubscription> {
    let link = sqlx::query(
        "SELECT tenant_id, user_id
         FROM clash_haitun_links
         WHERE token = $1::uuid
           AND revoked_at IS NULL",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?
    .ok_or_else(public_not_found)?;
    let tenant_id: String = link.try_get("tenant_id")?;
    let user_id: String = link.try_get("user_id")?;
    let serving = crate::serving::load_subscription_serving_projection(pool).await?;
    if !serving
        .snapshot
        .users
        .iter()
        .any(|user| user.tenant == tenant_id && user.id == user_id)
    {
        return Err(public_not_found());
    }
    serving.ensure_available()?;

    build_dynamic_clash(
        pool,
        &serving.snapshot,
        &tenant_id,
        &user_id,
        filter,
        DynamicClashTemplate::Haitun,
        true,
    )
    .await
}

/// The administrator-side lookup uses the same serving projection as the public endpoint. A
/// tenant administrator can obtain URLs only for users in their scope, and cannot accidentally
/// validate a URL against a committed-but-unpublished user.
pub async fn clash_subscription_for_user(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<DynamicClashSubscription> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    actor.require_tenant_access(&tenant_id, "Clash subscription")?;
    let serving = crate::serving::load_subscription_serving_projection(pool).await?;
    if !serving
        .snapshot
        .users
        .iter()
        .any(|user| user.tenant == tenant_id && user.id == user_id)
    {
        return Err(StoreError::NotFound(format!("user {tenant_id}/{user_id}")));
    }
    serving.ensure_available()?;
    build_dynamic_clash(
        pool,
        &serving.snapshot,
        &tenant_id,
        &user_id,
        SubscriptionFilter::default(),
        DynamicClashTemplate::Standard,
        false,
    )
    .await
}

/// Read the durable link separately from the generated subscription. A missing row means the
/// operator has never generated a Haitun URL; a row with `revoked_at` preserves that useful UI
/// distinction without leaving the old token live.
pub async fn clash_haitun_link_for_user(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<Option<ClashHaitunLink>> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    require_speedtest_subscription_admin(actor, &tenant_id)?;
    let row = sqlx::query(
        "SELECT tenant_id, user_id, token::text AS token,
                created_at::text AS created_at, revoked_at::text AS revoked_at
         FROM clash_haitun_links
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?;
    row.map(clash_haitun_link_from_row).transpose()
}

/// Generate the first link, return an existing active one idempotently, or replace a revoked
/// token. This is operational sharing state: it neither creates a revision nor changes anything
/// deployed to the agents.
pub async fn issue_clash_haitun_link(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<ClashHaitunLink> {
    write_clash_haitun_link(pool, actor, tenant_id, user_id, false).await
}

/// Atomically replace the URL token, including an active one. Already downloaded node
/// credentials are unchanged: invalidating them requires UUID rotation and grant publication.
pub async fn regenerate_clash_haitun_link(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<ClashHaitunLink> {
    write_clash_haitun_link(pool, actor, tenant_id, user_id, true).await
}

async fn write_clash_haitun_link(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    regenerate: bool,
) -> Result<ClashHaitunLink> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    require_speedtest_subscription_admin(actor, &tenant_id)?;
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM users
             WHERE tenant_id = $1 AND id = $2 AND status = 'active'
         )",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .fetch_one(pool)
    .await?;
    if !exists {
        return Err(StoreError::NotFound(format!(
            "active user {tenant_id}/{user_id}"
        )));
    }

    let token = generate_uuid_v4()?;
    let row = sqlx::query(
        "INSERT INTO clash_haitun_links (tenant_id, user_id, token)
         VALUES ($1, $2, $3::uuid)
         ON CONFLICT (tenant_id, user_id) DO UPDATE SET
             token = CASE
                 WHEN NOT $4 AND clash_haitun_links.revoked_at IS NULL THEN clash_haitun_links.token
                 ELSE EXCLUDED.token
             END,
             created_at = CASE
                 WHEN NOT $4 AND clash_haitun_links.revoked_at IS NULL THEN clash_haitun_links.created_at
                 ELSE EXCLUDED.created_at
             END,
             revoked_at = NULL
         RETURNING tenant_id, user_id, token::text AS token,
                   created_at::text AS created_at, revoked_at::text AS revoked_at",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(token)
    .bind(regenerate)
    .fetch_one(pool)
    .await?;
    clash_haitun_link_from_row(row)
}

fn require_speedtest_subscription_admin(actor: &AdminContext, tenant_id: &str) -> Result<()> {
    if !actor.can_manage_speedtest_subscriptions() {
        return Err(StoreError::Forbidden(
            "speed-test subscriptions require an administrator".to_owned(),
        ));
    }
    actor.require_tenant_access(tenant_id, "speed-test subscription")
}

fn clash_haitun_link_from_row(row: sqlx::postgres::PgRow) -> Result<ClashHaitunLink> {
    Ok(ClashHaitunLink {
        tenant_id: row.try_get("tenant_id")?,
        user_id: row.try_get("user_id")?,
        token: row.try_get("token")?,
        created_at: row.try_get("created_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

async fn build_dynamic_clash(
    pool: &PgPool,
    snapshot: &brocade_core::model::ModelSnapshot,
    tenant_id: &str,
    user_id: &str,
    filter: SubscriptionFilter,
    template: DynamicClashTemplate,
    hide_reason: bool,
) -> Result<DynamicClashSubscription> {
    let output = crate::compile_cache::compile_incremental(snapshot);
    let mut plan = crate::compile_cache::project_user(&output, tenant_id, user_id)
        .map(|plan| plan.as_ref().clone())
        .map_err(|blocked| {
            StoreError::InvalidData(format!(
                "cannot project user {tenant_id}/{user_id}: {blocked:?}"
            ))
        })?;
    if plan.entries.is_empty() {
        let authorized = snapshot.apps.iter().any(|app| {
            app.grants
                .iter()
                .any(|grant| grant.tenant == tenant_id && grant.user == user_id)
        });
        return Err(if authorized {
            StoreError::Unavailable(format!("user {tenant_id}/{user_id} 暂时无可用入口"))
        } else if hide_reason {
            public_not_found()
        } else {
            StoreError::NotFound(format!(
                "user {tenant_id}/{user_id} has no effective subscription"
            ))
        });
    }
    plan.retain_filter(filter);

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
    let mut artifact = subscription::build(&plan);
    artifact.mark_self_signed(&crate::cert::self_signed_certificate_pins(pool).await?);
    let (content, format) = render_subscription(&artifact, template);
    let usage = subscription_usage(pool, tenant_id, user_id, &app_ids).await?;
    Ok(DynamicClashSubscription {
        tenant_id: tenant_id.to_owned(),
        user_id: user_id.to_owned(),
        uuid,
        revision: snapshot.revision,
        content,
        format,
        usage,
    })
}

fn render_subscription(
    artifact: &subscription::Subscription,
    template: DynamicClashTemplate,
) -> (String, DynamicSubscriptionFormat) {
    match template {
        DynamicClashTemplate::Standard => (
            yaml::clash_subscription(artifact),
            DynamicSubscriptionFormat::Clash,
        ),
        DynamicClashTemplate::Haitun => (
            yaml::clash_haitun_subscription(artifact),
            DynamicSubscriptionFormat::Clash,
        ),
        DynamicClashTemplate::Shadowrocket => match uri::complete_subscription(artifact) {
            Ok(content) => (content, DynamicSubscriptionFormat::Shadowrocket),
            Err(reason) => (
                yaml::clash_subscription(artifact),
                DynamicSubscriptionFormat::ClashCompatibility(reason),
            ),
        },
    }
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
             FROM usage_rollups s, bounds b
             WHERE s.tenant_id = $1
               AND s.user_id = $2
               AND s.app_id = ANY($3::text[])
               AND s.period_start >= b.month_start
               AND s.period_start < b.month_end
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

#[cfg(test)]
mod tests {
    use super::*;
    use brocade_core::artifacts::subscription::{
        Subscription, SubscriptionEntry, SubscriptionSecurity, SubscriptionStream, SubscriptionTls,
    };

    fn artifact() -> Subscription {
        Subscription {
            tenant: "test".to_owned(),
            user: "alice".to_owned(),
            entries: vec![SubscriptionEntry {
                name: "Test node".to_owned(),
                server: "node.example".to_owned(),
                port: 443,
                uuid: "test-only".to_owned(),
                front_name: None,
                stream: SubscriptionStream::Tcp,
                security: SubscriptionSecurity::Tls(SubscriptionTls {
                    server_name: "node.example".to_owned(),
                    flow: None,
                    self_signed: false,
                    certificate_fingerprint: None,
                }),
            }],
            external_proxies: Vec::new(),
            front_groups: Vec::new(),
        }
    }

    #[test]
    fn subscription_templates_preserve_yaml_and_negotiate_only_complete_uris() {
        let mut artifact = artifact();
        for template in [DynamicClashTemplate::Standard, DynamicClashTemplate::Haitun] {
            let (content, format) = render_subscription(&artifact, template);
            assert_eq!(format, DynamicSubscriptionFormat::Clash);
            let expected = match template {
                DynamicClashTemplate::Haitun => yaml::clash_haitun_subscription(&artifact),
                _ => yaml::clash_subscription(&artifact),
            };
            assert_eq!(content, expected);
        }
        let (content, format) = render_subscription(&artifact, DynamicClashTemplate::Shadowrocket);
        assert_eq!(format, DynamicSubscriptionFormat::Shadowrocket);
        assert_eq!(content, uri::complete_subscription(&artifact).unwrap());

        if let SubscriptionSecurity::Tls(tls) = &mut artifact.entries[0].security {
            tls.self_signed = true;
            tls.certificate_fingerprint = Some("AB".repeat(32));
        }
        let (content, format) = render_subscription(&artifact, DynamicClashTemplate::Shadowrocket);
        assert_eq!(
            format,
            DynamicSubscriptionFormat::ClashCompatibility(
                uri::UriSubscriptionUnsupported::SelfSignedCertificate
            )
        );
        assert_eq!(content, yaml::clash_subscription(&artifact));
        assert!(content.contains(&format!("fingerprint: {}", "AB".repeat(32))));
        assert!(!content.contains("skip-cert-verify"));

        artifact.entries[0].front_name = Some("Front".to_owned());
        artifact
            .front_groups
            .push(subscription::SubscriptionFrontGroup {
                name: "Front".to_owned(),
                strategy: brocade_core::model::FrontStrategy::Select,
                members: vec!["DIRECT".to_owned()],
            });
        let (content, format) = render_subscription(&artifact, DynamicClashTemplate::Shadowrocket);
        assert_eq!(
            format,
            DynamicSubscriptionFormat::ClashCompatibility(
                uri::UriSubscriptionUnsupported::FrontProxy
            )
        );
        assert_eq!(content, yaml::clash_subscription(&artifact));
        assert!(content.contains("dialer-proxy: \"Front\""));
    }
}
