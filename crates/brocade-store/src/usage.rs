use std::collections::BTreeMap;

use brocade_core::model::parse_grant_label;

use brocade_deployment::protocol::{
    UsageChainSample, UsageMonthlySummary, UsageMonthlyViewRow, UsageNodeBucket, UsageNodeSeries,
    UsageNodeSeriesList, UsageReportRequest, UsageReportResult, UsageSample, UsageSampleList,
};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

/// How far the agent's reported instant may diverge from the control plane's clock and still
/// count. Beyond it the whole round is refused — the window boundary decides which month these
/// bytes land in, and month boundaries are where bills divide.
const MAX_CLOCK_SKEW_SECS: i64 = 600;

pub async fn record_usage_report(
    pool: &PgPool,
    node_id: &str,
    request: UsageReportRequest,
) -> Result<UsageReportResult> {
    if request.read_at_unix_secs <= 0 || request.xray_started_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "usage report timestamps must be positive unix seconds".to_owned(),
        ));
    }
    if request.read_at_unix_secs < request.xray_started_at_unix_secs {
        return Err(StoreError::InvalidData(
            "usage report read_at must not be before xray_started_at".to_owned(),
        ));
    }

    // Window boundaries all come from the unix seconds the agent reported, and month boundaries
    // are exactly where bills divide — a machine whose clock jumps into next month puts this
    // round's traffic on next month's bill. Nobody has to act maliciously for this; a broken NTP
    // suffices. So it is checked against the control plane's own clock first and the whole round
    // refused where they diverge too far: better to take nothing than to take it into the wrong
    // month. The 10-minute threshold is far above normal jitter (a 30s agent round plus network
    // plus spool replay), and small enough not to cross any accounting boundary but a month
    // boundary.
    let (skew_secs,): (i64,) =
        sqlx::query_as("SELECT abs(extract(epoch FROM now())::bigint - $1::bigint)")
            .bind(request.read_at_unix_secs)
            .fetch_one(pool)
            .await?;
    if skew_secs > MAX_CLOCK_SKEW_SECS {
        return Err(StoreError::InvalidData(format!(
            "usage report clock skew {skew_secs}s exceeds {MAX_CLOCK_SKEW_SECS}s; check the node's clock"
        )));
    }

    let mut tx = pool.begin().await?;
    let metadata = usage_metadata_for_node(&mut tx, node_id).await?;
    let mut accepted_readings = 0_u64;
    let mut inserted_samples = 0_u64;
    let mut skipped_counters = 0_u64;
    let mut rejected_counters = 0_u64;
    let mut gap_samples = 0_u64;
    let route = request.route.clone();

    for counter in request.counters {
        let label = counter.label.trim().to_owned();
        if label.is_empty() {
            skipped_counters += 1;
            continue;
        }
        let Ok(uplink_bytes) = u64_to_i64("uplink_bytes", counter.uplink_bytes) else {
            skipped_counters += 1;
            continue;
        };
        let Ok(downlink_bytes) = u64_to_i64("downlink_bytes", counter.downlink_bytes) else {
            skipped_counters += 1;
            continue;
        };
        let owner = match counter_by_label(&mut tx, node_id, &label).await? {
            CounterLookup::User(grant) => CounterOwner::User(grant),
            CounterLookup::ChainHop(hop) => CounterOwner::ChainHop(hop),
            CounterLookup::Unknown => {
                skipped_counters += 1;
                continue;
            }
            CounterLookup::ForeignNode => {
                // The node is reporting traffic for an ingress or relay hop it does not carry.
                // Not noise but an anomaly signal, counted separately.
                rejected_counters += 1;
                continue;
            }
        };

        let previous =
            previous_reading_for_update(&mut tx, node_id, &label, request.read_at_unix_secs)
                .await?;
        let inserted = insert_usage_reading(
            &mut tx,
            node_id,
            &label,
            request.read_at_unix_secs,
            request.xray_started_at_unix_secs,
            uplink_bytes,
            downlink_bytes,
        )
        .await?;
        if !inserted {
            skipped_counters += 1;
            continue;
        }
        accepted_readings += 1;

        if let Some(previous) = previous {
            if request.read_at_unix_secs <= previous.read_at_unix_secs {
                continue;
            }

            // Whether xray restarted is what the counters say, not what the clock says. They only
            // ever climb while one process lives, so a reading below its predecessor is the proof
            // that a different process is behind them — and right after that restart the absolute
            // value *is* the traffic since it came up.
            //
            // The reported start time deliberately does not get a vote. The node derives it from
            // whichever process is named `xray`, and the e2e prober spawns short-lived `xray`
            // children of its own; picking one of those makes a server that never stopped look
            // restarted every few minutes. Booking the whole cumulative counter each time that
            // happens is how one ingress grew 682 GiB of traffic in three days that nobody sent.
            // Counters that still climb mean nothing was missed, whatever timestamp came with them.
            let restarted =
                uplink_bytes < previous.uplink_bytes || downlink_bytes < previous.downlink_bytes;
            let (window_start, uplink_delta, downlink_delta, has_gap) = if !restarted {
                (
                    previous.read_at_unix_secs,
                    uplink_bytes - previous.uplink_bytes,
                    downlink_bytes - previous.downlink_bytes,
                    false,
                )
            } else {
                (
                    previous
                        .read_at_unix_secs
                        .max(request.xray_started_at_unix_secs),
                    uplink_bytes,
                    downlink_bytes,
                    true,
                )
            };

            if window_start < request.read_at_unix_secs {
                let insert = UsageSampleInsert {
                    node_id,
                    owner: &owner,
                    window_start_unix_secs: window_start,
                    window_end_unix_secs: request.read_at_unix_secs,
                    uplink_bytes: uplink_delta,
                    downlink_bytes: downlink_delta,
                    has_gap,
                    revision_id: metadata.revision_id,
                    deployment_id: metadata.deployment_id,
                };
                let sample_inserted = match &owner {
                    CounterOwner::User(_) => insert_usage_sample(&mut tx, insert).await?,
                    CounterOwner::ChainHop(_) => insert_chain_sample(&mut tx, insert).await?,
                };
                if sample_inserted {
                    inserted_samples += 1;
                    if has_gap {
                        gap_samples += 1;
                    }
                }
            }
        }
    }

    sqlx::query(
        "INSERT INTO node_agent_state (
            node_id, last_usage_report_at, xray_started_at, route_ipv4, route_ipv6
         )
         VALUES (
            $1, now(), to_timestamp($2::double precision),
            CASE WHEN $3::boolean THEN $4 ELSE NULL END,
            CASE WHEN $3::boolean THEN $5 ELSE NULL END
         )
         ON CONFLICT (node_id) DO UPDATE SET
            last_usage_report_at = EXCLUDED.last_usage_report_at,
            xray_started_at = EXCLUDED.xray_started_at,
            route_ipv4 = CASE WHEN $3::boolean THEN EXCLUDED.route_ipv4 ELSE node_agent_state.route_ipv4 END,
            route_ipv6 = CASE WHEN $3::boolean THEN EXCLUDED.route_ipv6 ELSE node_agent_state.route_ipv6 END",
    )
    .bind(node_id)
    .bind(request.xray_started_at_unix_secs)
    .bind(route.is_some())
    .bind(route.as_ref().and_then(|route| route.ipv4.as_deref()))
    .bind(route.as_ref().and_then(|route| route.ipv6.as_deref()))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(UsageReportResult {
        node_id: node_id.to_owned(),
        accepted_readings,
        inserted_samples,
        skipped_counters,
        rejected_counters,
        gap_samples,
    })
}

/// What the bar chart at the right of the machine list and its "this month" reading need: a
/// per-machine aggregated usage series plus the month's running total.
///
/// Why `/usage/samples` is not reused: that returns detail rows (one row = one window × one
/// grant), so sixteen machines over ten minutes is `16 × 20 × users × ingresses` rows against a
/// limit capped at 500. Aggregating here yields everything the page needs in one query.
///
/// It goes through the `node_usage_windows` view (the machine-dimension union of both sample
/// families) rather than touching the two underlying tables: "how much did this machine carry"
/// has to count both an ingress's user traffic and a relay's link hops — ingress and relay are
/// merely one machine's roles on different chains. The first version queried only
/// `usage_samples`, and every relay machine's bar chart was empty.
///
/// `window_secs` sets how long the series is (not the bucket width — the buckets are the agent's
/// reporting windows themselves).
pub async fn list_usage_node_series(
    pool: &PgPool,
    actor: &AdminContext,
    window_secs: u32,
) -> Result<UsageNodeSeriesList> {
    // Capped at 24 hours: anything longer belongs in a rollup table rather than scanning
    // detail rows window by window.
    let window_secs = i64::from(window_secs.clamp(60, 86_400));
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();

    // Month boundaries follow list_monthly_usage_summary: calendar months at +08, decided
    // server-side and never sent by the UI. wall_start is the human-facing wall-clock string,
    // inst_start carries the offset and is used only for filtering.
    let (wall_month_start, inst_month_start): (String, String) = sqlx::query_as(
        "SELECT to_char(date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong'),
                        'YYYY-MM-DD HH24:MI:SS'),
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                 AT TIME ZONE 'Asia/Hong_Kong')::text",
    )
    .fetch_one(pool)
    .await?;

    let (since,): (String,) =
        sqlx::query_as("SELECT (now() - make_interval(secs => $1::double precision))::text")
            .bind(window_secs as f64)
            .fetch_one(pool)
            .await?;

    // The window series. The two tables are UNIONed and aggregated by (node_id, window_end) —
    // window_end is the right edge of the agent's reporting window, both sides write the same
    // boundary for one report, and no further bucketing is needed. User traffic and relay
    // traffic are given in separate columns (FILTER): an ingress machine's bytes belong to a
    // user, a relay machine's belong to a link hop with no user dimension, and querying only
    // usage_samples leaves a relay machine's bar chart forever empty — while it is plainly
    // forwarding.
    let bucket_rows = sqlx::query(
        "SELECT node_id,
                window_end::text AS window_end,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind = 'user'), 0)::bigint
                    AS user_uplink_bytes,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind = 'user'), 0)::bigint
                    AS user_downlink_bytes,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind = 'relay'), 0)::bigint
                    AS relay_uplink_bytes,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind = 'relay'), 0)::bigint
                    AS relay_downlink_bytes
         FROM node_usage_windows
         WHERE window_end >= $1::timestamptz
           AND ($2::text IS NULL OR tenant_id = $2 OR tenant_id LIKE $3 ESCAPE '\\')
         GROUP BY node_id, window_end
         ORDER BY node_id, window_end",
    )
    .bind(&since)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;

    let month_rows = sqlx::query(
        "SELECT node_id,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind = 'user'), 0)::bigint
                    AS user_uplink_bytes,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind = 'user'), 0)::bigint
                    AS user_downlink_bytes,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind = 'relay'), 0)::bigint
                    AS relay_uplink_bytes,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind = 'relay'), 0)::bigint
                    AS relay_downlink_bytes,
                bool_or(has_gap) AS has_gap
         FROM node_usage_windows
         WHERE window_start >= $1::timestamptz
           AND ($2::text IS NULL OR tenant_id = $2 OR tenant_id LIKE $3 ESCAPE '\\')
         GROUP BY node_id",
    )
    .bind(&inst_month_start)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;

    // The two queries merge on node_id. A machine with a monthly total but no recent samples
    // (it ran this month and just stopped) must still appear in the result, or the UI treats it
    // as never enrolled.
    let blank = |node_id: String| UsageNodeSeries {
        node_id,
        buckets: Vec::new(),
        month_user_uplink_bytes: 0,
        month_user_downlink_bytes: 0,
        month_relay_uplink_bytes: 0,
        month_relay_downlink_bytes: 0,
        month_has_gap: false,
    };
    let mut by_node: BTreeMap<String, UsageNodeSeries> = BTreeMap::new();
    for row in &bucket_rows {
        let node_id: String = row.try_get("node_id")?;
        by_node
            .entry(node_id.clone())
            .or_insert_with(|| blank(node_id))
            .buckets
            .push(UsageNodeBucket {
                window_end: row.try_get("window_end")?,
                user_uplink_bytes: i64_to_u64(
                    "user_uplink_bytes",
                    row.try_get("user_uplink_bytes")?,
                )?,
                user_downlink_bytes: i64_to_u64(
                    "user_downlink_bytes",
                    row.try_get("user_downlink_bytes")?,
                )?,
                relay_uplink_bytes: i64_to_u64(
                    "relay_uplink_bytes",
                    row.try_get("relay_uplink_bytes")?,
                )?,
                relay_downlink_bytes: i64_to_u64(
                    "relay_downlink_bytes",
                    row.try_get("relay_downlink_bytes")?,
                )?,
            });
    }
    for row in &month_rows {
        let node_id: String = row.try_get("node_id")?;
        let entry = by_node
            .entry(node_id.clone())
            .or_insert_with(|| blank(node_id));
        entry.month_user_uplink_bytes =
            i64_to_u64("user_uplink_bytes", row.try_get("user_uplink_bytes")?)?;
        entry.month_user_downlink_bytes =
            i64_to_u64("user_downlink_bytes", row.try_get("user_downlink_bytes")?)?;
        entry.month_relay_uplink_bytes =
            i64_to_u64("relay_uplink_bytes", row.try_get("relay_uplink_bytes")?)?;
        entry.month_relay_downlink_bytes =
            i64_to_u64("relay_downlink_bytes", row.try_get("relay_downlink_bytes")?)?;
        entry.month_has_gap = row.try_get("has_gap")?;
    }

    Ok(UsageNodeSeriesList {
        since,
        month_start: wall_month_start,
        nodes: by_node.into_values().collect(),
    })
}

/// Delete raw readings past their retention.
///
/// `usage_readings` has one purpose: serving as the baseline for the next difference
/// (`previous_reading_for_update` takes the most recent one). But it appends a row per label
/// every 30 seconds and never reclaims — thirty machines with a hundred users each comes to over
/// two hundred million rows a month.
///
/// Seven days rather than only the most recent row: the difference needs one, and the extra week
/// is so that accounts can be reconciled after an incident (when a window's figure does not add
/// up, the raw cumulative values are the only thing that can reconstruct the truth).
///
/// Repetition is safe and several control-plane instances running at once is fine — DELETE is
/// idempotent.
pub async fn prune_usage_readings(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let retain_days = i64::from(retain_days.clamp(1, 365));
    let result = sqlx::query(
        "DELETE FROM usage_readings
         WHERE read_at < now() - make_interval(days => $1::int)",
    )
    .bind(retain_days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn list_usage_samples(
    pool: &PgPool,
    actor: &AdminContext,
    limit: u32,
    tenant_id: Option<&str>,
    user_id: Option<&str>,
    node_id: Option<&str>,
) -> Result<UsageSampleList> {
    let limit = i64::from(limit.clamp(1, 500));
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();
    let rows = sqlx::query(
        "SELECT id,
                sampled_at::text AS sampled_at,
                window_start::text AS window_start,
                window_end::text AS window_end,
                node_id,
                tenant_id,
                user_id,
                ingress_id,
                grant_label,
                uplink_bytes,
                downlink_bytes,
                has_gap,
                revision_id,
                deployment_id
         FROM usage_samples
         WHERE ($2::text IS NULL OR tenant_id = $2)
           AND ($3::text IS NULL OR user_id = $3)
           AND ($4::text IS NULL OR node_id = $4)
           AND (
                $5::text IS NULL
                OR tenant_id = $5
                OR tenant_id LIKE $6 ESCAPE '\\'
           )
         ORDER BY window_end DESC, id DESC
         LIMIT $1",
    )
    .bind(limit)
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;

    let samples = rows
        .iter()
        .map(|row| {
            let revision_id = row
                .try_get::<Option<i64>, _>("revision_id")?
                .map(revision_to_u64)
                .transpose()?;
            Ok(UsageSample {
                id: row.try_get("id")?,
                sampled_at: row.try_get("sampled_at")?,
                window_start: row.try_get("window_start")?,
                window_end: row.try_get("window_end")?,
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                ingress_id: row.try_get("ingress_id")?,
                grant_label: row.try_get("grant_label")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
                revision_id,
                deployment_id: row.try_get("deployment_id")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // Link hops are returned alongside user samples but in separate fields. Filtering by user_id
    // returns none of them: that filter means "how much did this person use", and link overhead
    // belongs to nobody.
    let chain_samples = if user_id.is_some() {
        Vec::new()
    } else {
        list_chain_samples(
            pool,
            limit,
            tenant_id,
            node_id,
            tenant_scope,
            &tenant_pattern,
        )
        .await?
    };

    Ok(UsageSampleList {
        samples,
        chain_samples,
    })
}

/// The calendar-month rollup: one row per user across all their access points (usage_samples
/// writes only granted rows, so a per-user sum is naturally the traffic of their view's access
/// points). Months are the calendar months of the control plane's local zone (+08), decided
/// server-side and never sent by the UI. Tenant-subtree scoping matches
/// list_usage_samples.
pub async fn list_monthly_usage_summary(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<UsageMonthlySummary> {
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();
    // Month boundaries need two representations:
    //  - wall_*: +08 wall-clock strings, human-facing (the month_start/month_end response). A
    //    timestamptz's ::text cannot be handed over directly — that rendering follows the
    //    database session's timezone, and under a UTC session midnight on 1 August renders as
    //    "2026-07-31 16:00:00+00", from which the UI slices 2026-07.
    //  - inst_*: timestamptz text (carrying an offset, unambiguous to parse), used only to
    //    filter samples.
    let (wall_start, wall_end, inst_start, inst_end): (String, String, String, String) =
        sqlx::query_as(
            "SELECT to_char(date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong'),
                        'YYYY-MM-DD HH24:MI:SS'),
                to_char(date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                        + INTERVAL '1 month', 'YYYY-MM-DD HH24:MI:SS'),
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                 AT TIME ZONE 'Asia/Hong_Kong')::text,
                ((date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                  + INTERVAL '1 month') AT TIME ZONE 'Asia/Hong_Kong')::text",
        )
        .fetch_one(pool)
        .await?;
    // View attribution prefers the sample's own column (frozen at insert), falling back to
    // deriving it through a JOIN only for older samples. LEFT JOIN rather than INNER: where the
    // frozen column has a value, the group still resolves even after the ingress was deleted —
    // INNER would discard those samples entirely, presenting as consumption inexplicably losing
    // a chunk.
    let rows = sqlx::query(
        "SELECT s.tenant_id,
                s.user_id,
                coalesce(s.app_id, i.app_id) AS app_id,
                sum(s.uplink_bytes)::bigint AS uplink_bytes,
                sum(s.downlink_bytes)::bigint AS downlink_bytes,
                bool_or(s.has_gap) AS has_gap
         FROM usage_samples s
         LEFT JOIN ingresses i ON i.id = s.ingress_id
         WHERE s.window_start >= $1::timestamptz
           AND s.window_start < $2::timestamptz
           AND coalesce(s.app_id, i.app_id) IS NOT NULL
           AND (
                $3::text IS NULL
                OR s.tenant_id = $3
                OR s.tenant_id LIKE $4 ESCAPE '\\'
           )
         GROUP BY s.tenant_id, s.user_id, coalesce(s.app_id, i.app_id)
         ORDER BY s.tenant_id, s.user_id, coalesce(s.app_id, i.app_id)",
    )
    .bind(&inst_start)
    .bind(&inst_end)
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;

    let views = rows
        .iter()
        .map(|row| {
            Ok(UsageMonthlyViewRow {
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                app_id: row.try_get("app_id")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(UsageMonthlySummary {
        month_start: wall_start,
        month_end: wall_end,
        views,
    })
}

async fn list_chain_samples(
    pool: &PgPool,
    limit: i64,
    tenant_id: Option<&str>,
    node_id: Option<&str>,
    tenant_scope: Option<&str>,
    tenant_pattern: &Option<String>,
) -> Result<Vec<UsageChainSample>> {
    let rows = sqlx::query(
        "SELECT id,
                sampled_at::text AS sampled_at,
                window_start::text AS window_start,
                window_end::text AS window_end,
                node_id,
                tenant_id,
                app_id,
                chain_id,
                hop_label,
                uplink_bytes,
                downlink_bytes,
                has_gap,
                revision_id,
                deployment_id
         FROM usage_chain_samples
         WHERE ($2::text IS NULL OR tenant_id = $2)
           AND ($3::text IS NULL OR node_id = $3)
           AND (
                $4::text IS NULL
                OR tenant_id = $4
                OR tenant_id LIKE $5 ESCAPE '\\'
           )
         ORDER BY window_end DESC, id DESC
         LIMIT $1",
    )
    .bind(limit)
    .bind(tenant_id)
    .bind(node_id)
    .bind(tenant_scope)
    .bind(tenant_pattern)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            let revision_id = row
                .try_get::<Option<i64>, _>("revision_id")?
                .map(revision_to_u64)
                .transpose()?;
            Ok(UsageChainSample {
                id: row.try_get("id")?,
                sampled_at: row.try_get("sampled_at")?,
                window_start: row.try_get("window_start")?,
                window_end: row.try_get("window_end")?,
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                app_id: row.try_get("app_id")?,
                chain_id: row.try_get("chain_id")?,
                hop_label: row.try_get("hop_label")?,
                uplink_bytes: i64_to_u64("uplink_bytes", row.try_get("uplink_bytes")?)?,
                downlink_bytes: i64_to_u64("downlink_bytes", row.try_get("downlink_bytes")?)?,
                has_gap: row.try_get("has_gap")?,
                revision_id,
                deployment_id: row.try_get("deployment_id")?,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct UsageMetadata {
    revision_id: Option<i64>,
    deployment_id: Option<i64>,
}

async fn usage_metadata_for_node(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<UsageMetadata> {
    let row = sqlx::query(
        "SELECT COALESCE(d.revision_id, cs.current_revision) AS revision_id,
                s.source_deployment_id AS deployment_id
         FROM control_state cs
         LEFT JOIN node_applied_state s
           ON s.node_id = $1
         LEFT JOIN deployments d
           ON d.id = s.source_deployment_id
         WHERE cs.id = TRUE",
    )
    .bind(node_id)
    .fetch_one(&mut **tx)
    .await?;

    Ok(UsageMetadata {
        revision_id: row.try_get("revision_id")?,
        deployment_id: row.try_get("deployment_id")?,
    })
}

#[derive(Debug)]
struct GrantMapping {
    tenant_id: String,
    user_id: String,
    ingress_id: String,
    label: String,
}

/// A relay hop's attribution. There is no user — this hop's credential is `{chain}@{node}`, and
/// that family of labels has no per-user dimension to begin with.
#[derive(Debug)]
struct ChainHopMapping {
    tenant_id: String,
    app_id: String,
    chain_id: String,
    label: String,
    /// Which machine this hop counts against. Usually the reporting one, though not under
    /// `HopDial::Reverse`: there the bytes were read by the portal machine while the hop belongs
    /// to the bridge.
    node_id: String,
}

#[derive(Debug)]
enum CounterOwner {
    User(GrantMapping),
    ChainHop(ChainHopMapping),
}

#[derive(Debug)]
enum CounterLookup {
    /// The label has no unique corresponding grant or link hop in the model.
    Unknown,
    /// It corresponds to one, but not on the reporting node.
    ForeignNode,
    User(GrantMapping),
    ChainHop(ChainHopMapping),
}

/// Recognize both label families: `{user}@{tenant}#{ingress}` is a user grant, `{chain}@{node}`
/// the accept credential of one hop on a relay chain. The two share a namespace, so they are
/// tried in order and only an unrecognized label counts as Unknown.
///
/// Both families use one anti-forgery rule: only the node carrying a label may report traffic
/// for it. That matches what the compiler distributes (physical/node.rs filters by ingress.node
/// and step.node), so nothing is killed by mistake: a node only ever receives its own client
/// list and can only count these labels anyway.
async fn counter_by_label(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    label: &str,
) -> Result<CounterLookup> {
    // Split the label first and then query by primary key, rather than assembling labels in SQL
    // to compare. Assembly belongs in one place, brocade_core::model::grant_label (the same one
    // the compiler uses to render xray clients), and this side only splits — both sides used to
    // assemble the format, and editing one while forgetting the other left the books permanently
    // unreconcilable. It also turns a full table scan per counter into one primary-key lookup: a
    // machine has tens to hundreds of counters and a round arrives every 30 seconds, so that
    // scan is real.
    if let Some((user_id, tenant_id, ingress_id)) = parse_grant_label(label) {
        let rows = sqlx::query(
            "SELECT g.tenant_id, g.user_id, g.ingress_id, i.node_id
             FROM grants g
             JOIN ingresses i ON i.id = g.ingress_id
             WHERE g.tenant_id = $1 AND g.user_id = $2 AND g.ingress_id = $3
             LIMIT 2",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(ingress_id)
        .fetch_all(&mut **tx)
        .await?;

        if rows.len() == 1 {
            let row = &rows[0];
            if row.try_get::<String, _>("node_id")? != node_id {
                return Ok(CounterLookup::ForeignNode);
            }
            return Ok(CounterLookup::User(GrantMapping {
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                ingress_id: row.try_get("ingress_id")?,
                label: label.to_owned(),
            }));
        }
        // One (tenant, user, ingress) matching several rows means one grant hangs off several
        // apps. Attribution is impossible there (whose view do those bytes belong to?), so it
        // counts as unrecognized.
        if rows.len() > 1 {
            return Ok(CounterLookup::Unknown);
        }
    }

    // Match on accept_label directly rather than assembling `chain_id || '@' || node_id`: the
    // label is the value that landed on disk and the key of xray's statistics, while an assembled
    // one is merely a derivation that ought to equal it.
    let rows = sqlx::query(
        "SELECT c.tenant_id, c.app_id, s.chain_id, s.node_id
         FROM steps s
         JOIN chains c ON c.id = s.chain_id
         WHERE s.accept_label = $1
         LIMIT 2",
    )
    .bind(label)
    .fetch_all(&mut **tx)
    .await?;

    if rows.len() != 1 {
        return Ok(CounterLookup::Unknown);
    }
    let row = &rows[0];
    let chain_id: String = row.try_get("chain_id")?;
    let hop_node: String = row.try_get("node_id")?;
    // A label belonging to somebody else was reported, so which kind must be established first.
    // Under `HopDial::Reverse` the traffic is dialed up by the downstream and travels back along
    // that connection: it enters the portal machine's relay port carrying the downstream's
    // credential, so xray records the bytes against the portal while the label names the
    // downstream. The downstream, meanwhile, has nothing — traffic injected by the bridge carries
    // no user identity (the bridge rule in `artifacts/xray.rs` has an empty `users`). So neither
    // end reconciles under this variant: unrecognized, this hop's relay traffic is carried for
    // nothing.
    if hop_node != node_id && !reverse_portal_for(tx, node_id, &chain_id, &hop_node).await? {
        return Ok(CounterLookup::ForeignNode);
    }
    Ok(CounterLookup::ChainHop(ChainHopMapping {
        tenant_id: row.try_get("tenant_id")?,
        app_id: row.try_get("app_id")?,
        chain_id,
        label: label.to_owned(),
        // Attribution follows the hop rather than whoever read the counter: were one hop to
        // change accounting machines on changing its dial method, both the bar chart and the bill
        // would jump the instant it shipped, while the same machine goes on forwarding.
        node_id: hop_node,
    }))
}

/// Whether the reporting machine is `hop_node`'s reverse-access upstream (portal) on this chain.
///
/// The test reads the forward in `steps.rules` — `hops` is compile-time IR and never lands in
/// the database, while the dial method is written on the rule anyway: this machine's step having
/// a `forward → hop_node` whose `dial` is reverse says that that machine's relay traffic is by
/// design meant to enter through this machine's port.
async fn reverse_portal_for(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    chain_id: &str,
    hop_node: &str,
) -> Result<bool> {
    let (found,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1
            FROM steps s, jsonb_array_elements(s.rules) AS r
            WHERE s.chain_id = $1
              AND s.node_id = $2
              AND r->'a'->>'t' = 'forward'
              AND r->'a'->>'to' = $3
              AND r->'a'->'dial'->>'t' = 'reverse'
         )",
    )
    .bind(chain_id)
    .bind(node_id)
    .bind(hop_node)
    .fetch_one(&mut **tx)
    .await?;
    Ok(found)
}

/// The baseline the next reading is differenced against. No start time among the fields: the
/// counters alone decide whether xray restarted (see `record_usage_report`), and keeping a
/// timestamp here would invite that decision to be made on it again.
#[derive(Debug)]
struct PreviousReading {
    read_at_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
}

async fn previous_reading_for_update(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    label: &str,
    read_at_unix_secs: i64,
) -> Result<Option<PreviousReading>> {
    let row = sqlx::query(
        "SELECT EXTRACT(EPOCH FROM read_at)::bigint AS read_at_unix_secs,
                uplink_bytes,
                downlink_bytes
         FROM usage_readings
         WHERE node_id = $1
           AND label = $2
           AND read_at < to_timestamp($3::double precision)
         ORDER BY read_at DESC
         LIMIT 1
         FOR UPDATE",
    )
    .bind(node_id)
    .bind(label)
    .bind(read_at_unix_secs)
    .fetch_optional(&mut **tx)
    .await?;

    row.map(|row| {
        Ok(PreviousReading {
            read_at_unix_secs: row.try_get("read_at_unix_secs")?,
            uplink_bytes: row.try_get("uplink_bytes")?,
            downlink_bytes: row.try_get("downlink_bytes")?,
        })
    })
    .transpose()
}

async fn insert_usage_reading(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    label: &str,
    read_at_unix_secs: i64,
    xray_started_at_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
) -> Result<bool> {
    let result = sqlx::query(
        "INSERT INTO usage_readings (
            node_id, label, read_at, xray_started_at, uplink_bytes, downlink_bytes
         )
         VALUES (
            $1, $2,
            to_timestamp($3::double precision),
            to_timestamp($4::double precision),
            $5, $6
         )
         ON CONFLICT (node_id, label, read_at) DO NOTHING",
    )
    .bind(node_id)
    .bind(label)
    .bind(read_at_unix_secs)
    .bind(xray_started_at_unix_secs)
    .bind(uplink_bytes)
    .bind(downlink_bytes)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

struct UsageSampleInsert<'a> {
    node_id: &'a str,
    owner: &'a CounterOwner,
    window_start_unix_secs: i64,
    window_end_unix_secs: i64,
    uplink_bytes: i64,
    downlink_bytes: i64,
    has_gap: bool,
    revision_id: Option<i64>,
    deployment_id: Option<i64>,
}

async fn insert_usage_sample(
    tx: &mut Transaction<'_, Postgres>,
    sample: UsageSampleInsert<'_>,
) -> Result<bool> {
    let CounterOwner::User(grant) = sample.owner else {
        return Ok(false);
    };
    // app_id is looked up from ingresses once at insert and frozen, no longer following the
    // model: when an ingress is moved to another view, this row still says which view it counted
    // against at the time. Derived on demand, a quota's numerator would jump wholesale to the new
    // view the instant the model changed.
    // An ingress that cannot be found leaves NULL, and the query side falls back to a JOIN.
    let result = sqlx::query(
        "INSERT INTO usage_samples (
            window_start, window_end, node_id,
            tenant_id, user_id, ingress_id, app_id, grant_label,
            uplink_bytes, downlink_bytes, has_gap,
            revision_id, deployment_id
         )
         VALUES (
            to_timestamp($1::double precision),
            to_timestamp($2::double precision),
            $3, $4, $5, $6,
            (SELECT app_id FROM ingresses WHERE id = $6),
            $7, $8, $9, $10, $11, $12
         )
         ON CONFLICT (node_id, grant_label, window_start, window_end) DO NOTHING",
    )
    .bind(sample.window_start_unix_secs)
    .bind(sample.window_end_unix_secs)
    .bind(sample.node_id)
    .bind(&grant.tenant_id)
    .bind(&grant.user_id)
    .bind(&grant.ingress_id)
    .bind(&grant.label)
    .bind(sample.uplink_bytes)
    .bind(sample.downlink_bytes)
    .bind(sample.has_gap)
    .bind(sample.revision_id)
    .bind(sample.deployment_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

async fn insert_chain_sample(
    tx: &mut Transaction<'_, Postgres>,
    sample: UsageSampleInsert<'_>,
) -> Result<bool> {
    let CounterOwner::ChainHop(hop) = sample.owner else {
        return Ok(false);
    };
    // `hop.node_id` rather than `sample.node_id`: under reverse access the counter was read by
    // the portal machine while the hop is forwarded by the bridge. The raw reading is still
    // recorded against whoever read it (differences must be taken against one source), and only
    // this step, turning it into accounting, attributes it to the hop's owner.
    let result = sqlx::query(
        "INSERT INTO usage_chain_samples (
            window_start, window_end, node_id,
            tenant_id, app_id, chain_id, hop_label,
            uplink_bytes, downlink_bytes, has_gap,
            revision_id, deployment_id
         )
         VALUES (
            to_timestamp($1::double precision),
            to_timestamp($2::double precision),
            $3, $4, $5, $6, $7, $8, $9, $10, $11, $12
         )
         ON CONFLICT (node_id, hop_label, window_start, window_end) DO NOTHING",
    )
    .bind(sample.window_start_unix_secs)
    .bind(sample.window_end_unix_secs)
    .bind(&hop.node_id)
    .bind(&hop.tenant_id)
    .bind(&hop.app_id)
    .bind(&hop.chain_id)
    .bind(&hop.label)
    .bind(sample.uplink_bytes)
    .bind(sample.downlink_bytes)
    .bind(sample.has_gap)
    .bind(sample.revision_id)
    .bind(sample.deployment_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

fn u64_to_i64(field: &str, value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} is out of range")))
}

fn i64_to_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| StoreError::InvalidData(format!("{field} is out of range")))
}

fn revision_to_u64(revision: i64) -> Result<u64> {
    u64::try_from(revision)
        .map_err(|_| StoreError::InvalidData(format!("revision_id out of range: {revision}")))
}
