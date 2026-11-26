//! Serving client subscriptions. Pulls read a complete process-local materialization of the last
//! fully converged Serving projection and account the current user billing cycle. A replacement
//! generation is rendered in the background; publication and rendering never make the previous
//! complete generation unavailable. Administrator-only sharing links are separate operational
//! state.

#[cfg(test)]
use brocade_core::{
    artifacts::subscription,
    format::{uri, yaml},
};
use brocade_core::{model::IpFamily, physical::user::SubscriptionFilter};
use sqlx::{PgPool, Row};

use super::*;
use crate::{
    credentials::generate_uuid_v4, input::required_text,
    subscription_render::RenderedSubscriptionUser, AdminContext, PgStore, Result, StoreError,
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
    store: &PgStore,
    uuid: &str,
) -> Result<DynamicClashSubscription> {
    clash_subscription_by_uuid_filtered(store, uuid, SubscriptionFilter::default()).await
}

/// The family-specific public URLs are alternate views of the same serving projection. Keeping the
/// filter here ensures VLESS artifacts and Clash subscriptions both use `UserPlan::retain_family`
/// rather than growing subtly different notions of an IPv4/IPv6-capable entry.
pub async fn clash_subscription_by_uuid_for_family(
    store: &PgStore,
    uuid: &str,
    family: Option<IpFamily>,
) -> Result<DynamicClashSubscription> {
    clash_subscription_by_uuid_filtered(
        store,
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
    store: &PgStore,
    uuid: &str,
    filter: SubscriptionFilter,
) -> Result<DynamicClashSubscription> {
    subscription_by_uuid(store, uuid, filter, DynamicClashTemplate::Standard).await
}

pub async fn shadowrocket_subscription_by_uuid_filtered(
    store: &PgStore,
    uuid: &str,
    filter: SubscriptionFilter,
) -> Result<DynamicClashSubscription> {
    subscription_by_uuid(store, uuid, filter, DynamicClashTemplate::Shadowrocket).await
}

async fn subscription_by_uuid(
    store: &PgStore,
    uuid: &str,
    filter: SubscriptionFilter,
    template: DynamicClashTemplate,
) -> Result<DynamicClashSubscription> {
    let serving = store.rendered_subscriptions().await?;
    let user = serving.user_by_uuid(uuid).ok_or_else(public_not_found)?;
    build_rendered_clash(store.pool(), &user, filter, template, true).await
}

/// Resolve a separately revocable Haitun bearer. The lookup deliberately happens before model
/// projection: revoked, unknown, disabled and currently unusable users all have the same public
/// answer and disclose no account state.
pub async fn clash_subscription_by_haitun_token_for_family(
    store: &PgStore,
    token: &str,
    family: Option<IpFamily>,
) -> Result<DynamicClashSubscription> {
    clash_subscription_by_haitun_token_filtered(
        store,
        token,
        SubscriptionFilter {
            family,
            ..Default::default()
        },
    )
    .await
}

pub async fn clash_subscription_by_haitun_token_filtered(
    store: &PgStore,
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
    .fetch_optional(store.pool())
    .await?
    .ok_or_else(public_not_found)?;
    let tenant_id: String = link.try_get("tenant_id")?;
    let user_id: String = link.try_get("user_id")?;
    let user = store
        .rendered_subscriptions()
        .await?
        .user(&tenant_id, &user_id)
        .ok_or_else(public_not_found)?;
    build_rendered_clash(
        store.pool(),
        &user,
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
    store: &PgStore,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<DynamicClashSubscription> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    actor.require_tenant_access(&tenant_id, "Clash subscription")?;
    let user = store
        .rendered_subscriptions()
        .await?
        .user(&tenant_id, &user_id)
        .ok_or_else(|| StoreError::NotFound(format!("user {tenant_id}/{user_id}")))?;
    build_rendered_clash(
        store.pool(),
        &user,
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
                 WHEN clash_haitun_links.revoked_at IS NULL THEN clash_haitun_links.token
                 ELSE EXCLUDED.token
             END,
             created_at = CASE
                 WHEN clash_haitun_links.revoked_at IS NULL THEN clash_haitun_links.created_at
                 ELSE EXCLUDED.created_at
             END,
             revoked_at = NULL
         RETURNING tenant_id, user_id, token::text AS token,
                   created_at::text AS created_at, revoked_at::text AS revoked_at",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(token)
    .fetch_one(pool)
    .await?;
    clash_haitun_link_from_row(row)
}

/// Stop future pulls without minting a replacement or changing deployed node credentials.
/// Repeated revocation preserves the original timestamp; UUID rotation still needs publication.
pub async fn revoke_clash_haitun_link(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
) -> Result<ClashHaitunLink> {
    let tenant_id = required_text(tenant_id, "tenant_id")?;
    let user_id = required_text(user_id, "user id")?;
    require_speedtest_subscription_admin(actor, &tenant_id)?;
    let row = sqlx::query(
        "UPDATE clash_haitun_links
         SET revoked_at = COALESCE(revoked_at, now())
         WHERE tenant_id = $1 AND user_id = $2
         RETURNING tenant_id, user_id, token::text AS token,
                   created_at::text AS created_at, revoked_at::text AS revoked_at",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("Haitun link {tenant_id}/{user_id}")))?;
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

async fn build_rendered_clash(
    pool: &PgPool,
    user: &RenderedSubscriptionUser,
    filter: SubscriptionFilter,
    template: DynamicClashTemplate,
    hide_reason: bool,
) -> Result<DynamicClashSubscription> {
    // Manual authorization follows the serving checkpoint. Only the operational time policy
    // is checked live, so an unpublished manual disable cannot rewrite a stable subscription.
    let eligible: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2
          AND user_service_state(service_starts_at,service_expires_at,now())='active')",
    )
    .bind(&user.tenant_id)
    .bind(&user.user_id)
    .fetch_one(pool)
    .await?;
    if !eligible {
        return Err(if hide_reason {
            public_not_found()
        } else {
            StoreError::NotFound("用户服务尚未生效或已到期".into())
        });
    }
    if !user.has_entries() {
        return Err(if user.authorized {
            StoreError::Unavailable(format!(
                "user {}/{} 暂时无可用入口",
                user.tenant_id, user.user_id
            ))
        } else if hide_reason {
            public_not_found()
        } else {
            StoreError::NotFound(format!(
                "user {}/{} has no effective subscription",
                user.tenant_id, user.user_id
            ))
        });
    }
    if user.app_ids.is_empty() {
        return Err(if hide_reason {
            public_not_found()
        } else {
            StoreError::NotFound(format!(
                "user {}/{} has no effective subscription",
                user.tenant_id, user.user_id
            ))
        });
    }

    let rendered = user.variant(filter);
    let (content, format) = match template {
        DynamicClashTemplate::Standard => (
            rendered.clash_with_rules.clone(),
            DynamicSubscriptionFormat::Clash,
        ),
        DynamicClashTemplate::Haitun => (rendered.haitun.clone(), DynamicSubscriptionFormat::Clash),
        DynamicClashTemplate::Shadowrocket => (rendered.native.clone(), rendered.native_format),
    };
    let usage = subscription_usage(pool, &user.tenant_id, &user.user_id, &user.app_ids).await?;
    Ok(DynamicClashSubscription {
        tenant_id: user.tenant_id.clone(),
        user_id: user.user_id.clone(),
        uuid: user.uuid.clone(),
        revision: user.revision,
        content,
        format,
        usage,
    })
}

#[cfg(test)]
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
             SELECT c.starts_at AS cycle_start,c.ends_at AS cycle_end
             FROM users u CROSS JOIN LATERAL user_traffic_cycle(u.traffic_reset_day,now()) c
             WHERE u.tenant_id=$1 AND u.id=$2
         ), used AS (
             SELECT coalesce(sum(s.uplink_bytes), 0)::bigint AS upload_bytes,
                    coalesce(sum(s.downlink_bytes), 0)::bigint AS download_bytes,
                    coalesce(bool_or(s.has_gap), false) AS has_gap
             FROM bounds b CROSS JOIN LATERAL user_cycle_access_usage($1,$2,b.cycle_start,b.cycle_end) s
             WHERE s.app_id = ANY($3::text[])
         ), limits AS (
             SELECT count(*)::bigint AS limited_count,
                    coalesce(sum(q.limit_bytes), 0)::bigint AS total_bytes
             FROM user_app_quotas q
             WHERE q.tenant_id = $1
               AND q.user_id = $2
               AND q.app_id = ANY($3::text[])
         )
         SELECT u.upload_bytes, u.download_bytes, u.has_gap,
                owner.traffic_accounting_mode,
                (to_jsonb(owner.service_expires_at AT TIME ZONE 'UTC') #>> '{}') || 'Z' AS expires_at,
                CASE WHEN l.limited_count = cardinality($3::text[])
                     THEN l.total_bytes ELSE NULL END AS total_bytes,
                to_char(b.cycle_end AT TIME ZONE 'Asia/Hong_Kong',
                        'YYYY-MM-DD\"T\"HH24:MI:SS') || '+08:00' AS reset_at
         FROM used u
         CROSS JOIN limits l
         CROSS JOIN bounds b
         JOIN users owner ON owner.tenant_id = $1 AND owner.id = $2",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(app_ids)
    .fetch_one(pool)
    .await?;

    let multiplier = match row
        .try_get::<String, _>("traffic_accounting_mode")?
        .as_str()
    {
        "single" => 1,
        "double" => 2,
        other => {
            return Err(StoreError::InvalidData(format!(
                "unknown user traffic accounting mode {other}"
            )))
        }
    };
    let upload_bytes =
        nonnegative_bytes("upload_bytes", row.try_get("upload_bytes")?)?.saturating_mul(multiplier);
    let download_bytes = nonnegative_bytes("download_bytes", row.try_get("download_bytes")?)?
        .saturating_mul(multiplier);
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
        expires_at: row.try_get("expires_at")?,
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
