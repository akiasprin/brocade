//! Bounded optional observations: independent lock/transaction, never a billing receipt.
use crate::{AdminContext, Result, StoreError};
use brocade_deployment::other_domain::{
    valid_domain, OtherDomainPolicy, OtherDomainReport, UserOtherDomainRow, UserOtherDomains,
    DETAIL_CHECKPOINT_VERSION, OTHER_DOMAIN_MAX_ROWS, OTHER_DOMAIN_VERSION,
};
use brocade_deployment::site_usage::SiteUsagePeriod;
use brocade_deployment::sniffing_failed::{
    valid_target, SniffingFailedPolicy, SniffingFailedReport, UserSniffingFailedRow,
    UserSniffingFailedTargets,
};
use sqlx::{PgPool, Row};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

const DETAIL_HISTORY_MAX_ROWS: usize = 65_536;

// The existing namespace column identifies a cumulative counter lifetime. A segment suffix
// separates readmissions without a schema change or altering legacy v1 counter identities.
fn counter_epoch(epoch: &str, counter_id: u64) -> Cow<'_, str> {
    if counter_id == 0 {
        Cow::Borrowed(epoch)
    } else {
        Cow::Owned(format!("{epoch}:{counter_id}"))
    }
}

#[derive(Clone, Copy)]
enum DetailKind {
    Domain,
    FailedTarget,
}
impl DetailKind {
    fn history_table(self) -> &'static str {
        match self {
            Self::Domain => "user_other_domain_hourly",
            Self::FailedTarget => "user_sniffing_failed_hourly",
        }
    }
    // Only private, fixed SQL identifiers are selected here; every external value stays bound.
    fn sql(self, source: &'static str) -> Cow<'static, str> {
        match self {
            Self::Domain => Cow::Borrowed(source),
            Self::FailedTarget => Cow::Owned(
                source
                    .replace("other_domain", "sniffing_failed")
                    .replace("domain", "target")
                    .replace("1481785690", "1481785691"),
            ),
        }
    }
    fn key_column(self) -> &'static str {
        match self {
            Self::Domain => "domain",
            Self::FailedTarget => "target",
        }
    }
    fn valid_key(self, value: &str) -> bool {
        match self {
            Self::Domain => valid_domain(value),
            Self::FailedTarget => valid_target(value),
        }
    }
    fn decode_policy(self, value: serde_json::Value) -> Result<OtherDomainPolicy> {
        let policy = match self {
            Self::Domain => serde_json::from_value(value)?,
            Self::FailedTarget => serde_json::from_value::<SniffingFailedPolicy>(value)?.budget(),
        };
        policy
            .validate()
            .map_err(|e| StoreError::InvalidData(e.into()))?;
        Ok(policy)
    }
    fn encode_policy(self, value: OtherDomainPolicy) -> Result<serde_json::Value> {
        Ok(match self {
            Self::Domain => serde_json::to_value(value)?,
            Self::FailedTarget => serde_json::to_value(SniffingFailedPolicy::from_budget(value))?,
        })
    }
}

pub(crate) async fn policy(pool: &PgPool) -> Result<OtherDomainPolicy> {
    policy_for(pool, DetailKind::Domain).await
}
async fn policy_for(pool: &PgPool, kind: DetailKind) -> Result<OtherDomainPolicy> {
    let value = sqlx::query_scalar(
        &kind.sql("SELECT other_domain_policy FROM control_state WHERE id=TRUE"),
    )
    .fetch_one(pool)
    .await?;
    let policy = kind.decode_policy(value)?;
    policy
        .validate()
        .map_err(|e| StoreError::InvalidData(e.into()))?;
    Ok(policy)
}
pub(crate) async fn update_policy(
    pool: &PgPool,
    actor: &AdminContext,
    value: OtherDomainPolicy,
) -> Result<OtherDomainPolicy> {
    update_policy_for(pool, actor, value, DetailKind::Domain).await
}
async fn update_policy_for(
    pool: &PgPool,
    actor: &AdminContext,
    value: OtherDomainPolicy,
    kind: DetailKind,
) -> Result<OtherDomainPolicy> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can change domain observations".into(),
        ));
    }
    value
        .validate()
        .map_err(|e| StoreError::InvalidData(e.into()))?;
    sqlx::query(&kind.sql("UPDATE control_state SET other_domain_policy=$1 WHERE id=TRUE"))
        .bind(kind.encode_policy(value)?)
        .execute(pool)
        .await?;
    Ok(value)
}

#[cfg(test)]
fn validate(report: &OtherDomainReport, now: i64) -> Result<()> {
    validate_for(report, now, DetailKind::Domain)
}
fn validate_for(report: &OtherDomainReport, now: i64, kind: DetailKind) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut segments = BTreeSet::new();
    let checkpointed = report.version == DETAIL_CHECKPOINT_VERSION;
    if !matches!(
        report.version,
        OTHER_DOMAIN_VERSION | DETAIL_CHECKPOINT_VERSION
    ) || report.epoch.len() != 32
        || !report.epoch.bytes().all(|c| c.is_ascii_hexdigit())
        || report.counters.len() > OTHER_DOMAIN_MAX_ROWS
        || report.read_at_unix_secs > now + 60
        || (!checkpointed && report.read_at_unix_secs < now - 600)
        || report.activation_unix_secs <= 0
        || report.activation_unix_secs > report.read_at_unix_secs
        || report.usage_generation_id <= 0
    {
        return Err(StoreError::InvalidData(
            "invalid domain observation envelope".into(),
        ));
    }
    let hour = report.read_at_unix_secs / 3600 * 3600;
    for row in &report.counters {
        if row.label.is_empty()
            || row.label.len() > 256
            || !kind.valid_key(&row.domain)
            || ![hour, hour - 3600].contains(&row.bucket)
            || row.uplink_bytes > i64::MAX as u64
            || row.downlink_bytes > i64::MAX as u64
            || (checkpointed && (row.counter_id == 0 || row.counter_id > i64::MAX as u64))
            || (checkpointed && !segments.insert(row.counter_id))
            || (!checkpointed && row.counter_id != 0)
            || !seen.insert((row.bucket, &row.label, &row.domain, row.counter_id))
        {
            return Err(StoreError::InvalidData(
                "invalid or duplicate domain observation row".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) async fn record(pool: &PgPool, node: &str, report: OtherDomainReport) -> Result<()> {
    record_for(pool, node, report, DetailKind::Domain).await
}
async fn record_for(
    pool: &PgPool,
    node: &str,
    report: OtherDomainReport,
    kind: DetailKind,
) -> Result<()> {
    let history_hours = crate::retention::ttl_hours(pool, kind.history_table()).await?;
    let mut tx = pool.begin().await?;
    sqlx::query(&kind.sql("SET LOCAL statement_timeout='10s'"))
        .execute(&mut *tx)
        .await?;
    // This optional path neither acquires billing's row lock nor waits on another domain report.
    let locked: bool =
        sqlx::query_scalar(&kind.sql("SELECT pg_try_advisory_xact_lock(1481785690,hashtext($1))"))
            .bind(node)
            .fetch_one(&mut *tx)
            .await?;
    if !locked {
        return Err(StoreError::InvalidData(
            "domain observation busy; retry later".into(),
        ));
    }
    let now: i64 = sqlx::query_scalar(&kind.sql("SELECT extract(epoch FROM now())::bigint"))
        .fetch_one(&mut *tx)
        .await?;
    validate_for(&report, now, kind)?;
    let checkpointed = report.version == DETAIL_CHECKPOINT_VERSION;
    // Checkpoint replay and its deduplication lifetime are independent of history retention.
    let replay_floor = (now / 3600 - 23) * 3600;
    if checkpointed && report.read_at_unix_secs / 3600 * 3600 < replay_floor {
        // Already outside the checkpoint replay window. Acknowledge an expired durable
        // checkpoint so it cannot permanently pin the Agent's bounded FIFO.
        tx.commit().await?;
        return Ok(());
    }
    if !checkpointed {
        let checkpointed_epoch: bool = sqlx::query_scalar(&kind.sql("SELECT EXISTS (SELECT 1 FROM other_domain_counter_heads WHERE node_id=$1 AND epoch LIKE $2)"))
            .bind(node).bind(format!("{}:%", report.epoch)).fetch_one(&mut *tx).await?;
        if checkpointed_epoch {
            return Err(StoreError::InvalidData("checkpointed epoch requires v2 reports; restart Xray before legacy observation rollback".into()));
        }
    }
    let policy: serde_json::Value = sqlx::query_scalar(
        &kind.sql("SELECT other_domain_policy FROM control_state WHERE id=TRUE"),
    )
    .fetch_one(&mut *tx)
    .await?;
    let policy = kind.decode_policy(policy)?;
    policy
        .validate()
        .map_err(|e| StoreError::InvalidData(e.into()))?;
    if !policy.enabled && !checkpointed {
        return Err(StoreError::Forbidden(
            "domain observations are disabled".into(),
        ));
    }
    let previous = sqlx::query(&kind.sql(
        "SELECT epoch,read_at,observed_since,limited FROM node_other_domain_state WHERE node_id=$1",
    ))
    .bind(node)
    .fetch_optional(&mut *tx)
    .await?;
    if !checkpointed
        && previous
            .as_ref()
            .is_some_and(|p| p.get::<i64, _>("read_at") >= report.read_at_unix_secs)
    {
        tx.commit().await?;
        return Ok(());
    }
    if !checkpointed
        && previous.as_ref().is_some_and(|p| {
            p.get::<String, _>("epoch") != report.epoch
                && p.get::<i64, _>("observed_since") > report.activation_unix_secs
        })
    {
        return Err(StoreError::InvalidData(
            "stale domain observation epoch".into(),
        ));
    }
    let owners =
        crate::usage::other_domain_owners(&mut tx, node, report.usage_generation_id).await?;
    let mut first_segments = BTreeMap::<(i64, &str, &str), u64>::new();
    for row in &report.counters {
        first_segments
            .entry((row.bucket, row.label.as_str(), row.domain.as_str()))
            .and_modify(|id| *id = (*id).min(row.counter_id))
            .or_insert(row.counter_id);
    }
    let report_buckets = report
        .counters
        .iter()
        .map(|r| r.bucket)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut counter_epochs = report
        .counters
        .iter()
        .map(|r| counter_epoch(&report.epoch, r.counter_id).into_owned())
        .collect::<Vec<_>>();
    if checkpointed {
        counter_epochs.push(report.epoch.clone());
    }
    let old=sqlx::query(&kind.sql("SELECT bucket,label,domain,epoch,uplink_bytes,downlink_bytes FROM other_domain_counter_heads WHERE node_id=$1 AND epoch=ANY($2) AND bucket >= $3 AND bucket=ANY($4)"))
        .bind(node).bind(&counter_epochs).bind(replay_floor).bind(&report_buckets).fetch_all(&mut *tx).await?
        .into_iter().map(|r| ((r.get::<i64,_>("bucket"),r.get::<String,_>("label"),r.get::<String, _>(kind.key_column()),r.get::<String,_>("epoch")),(r.get::<i64,_>("uplink_bytes"),r.get::<i64,_>("downlink_bytes")))).collect::<BTreeMap<_,_>>();
    let mut limited = report.limited;
    let same_epoch = previous
        .as_ref()
        .is_some_and(|p| p.get::<String, _>("epoch") == report.epoch);
    let since = if same_epoch {
        previous
            .as_ref()
            .map(|p| p.get("observed_since"))
            .unwrap_or(report.activation_unix_secs)
    } else {
        report.activation_unix_secs
    };
    if let Some(p) = &previous {
        limited |= same_epoch
            && (p.get::<bool, _>("limited")
                || report.read_at_unix_secs - p.get::<i64, _>("read_at") > 3600);
    }
    // Arrays are bounded by the protocol and one batch avoids thousands of SQL round trips.
    let (mut buckets, mut labels, mut domains, mut epochs, mut ups, mut downs) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut totals = BTreeMap::<(i64, String, String, String), (i64, i64)>::new();
    // A restarted/misbehaving node cannot evade the per-hour storage budget by
    // rotating its volatile epoch or splitting a snapshot into many requests.
    let report_domains = report
        .counters
        .iter()
        .map(|r| r.domain.as_str())
        .collect::<Vec<_>>();
    let persisted=sqlx::query(&kind.sql("SELECT bucket,tenant_id,user_id,domain FROM user_other_domain_hourly WHERE node_id=$1 AND bucket=ANY($2) AND domain=ANY($3)"))
        .bind(node).bind(&report_buckets).bind(&report_domains).fetch_all(&mut *tx).await?;
    let mut persisted_keys: BTreeSet<_> = persisted
        .into_iter()
        .map(|r| {
            (
                r.get::<i64, _>("bucket"),
                r.get::<String, _>("tenant_id"),
                r.get::<String, _>("user_id"),
                r.get::<String, _>(kind.key_column()),
            )
        })
        .collect();
    let mut bucket_counts = sqlx::query(&kind.sql("SELECT bucket,count(*)::bigint AS n FROM user_other_domain_hourly WHERE node_id=$1 AND bucket=ANY($2) GROUP BY bucket"))
        .bind(node).bind(&report_buckets).fetch_all(&mut *tx).await?.into_iter()
        .map(|r| (r.get::<i64,_>("bucket"), r.get::<i64,_>("n") as usize)).collect::<BTreeMap<_,_>>();
    let mut head_counts = sqlx::query(&kind.sql("SELECT bucket,count(*)::bigint AS n FROM other_domain_counter_heads WHERE node_id=$1 AND bucket=ANY($2) GROUP BY bucket"))
        .bind(node).bind(&report_buckets).fetch_all(&mut *tx).await?.into_iter()
        .map(|r| (r.get::<i64,_>("bucket"), r.get::<i64,_>("n") as usize)).collect::<BTreeMap<_,_>>();
    for r in &report.counters {
        if checkpointed && r.bucket < replay_floor {
            continue;
        }
        let Some((tenant, user)) = owners.get(&r.label) else {
            limited = true;
            continue;
        };
        let head_key = (
            r.bucket,
            r.label.clone(),
            r.domain.clone(),
            counter_epoch(&report.epoch, r.counter_id).into_owned(),
        );
        let previous_head = old.get(&head_key);
        let legacy_key = (
            r.bucket,
            r.label.clone(),
            r.domain.clone(),
            report.epoch.clone(),
        );
        let first_segment = first_segments.get(&(r.bucket, r.label.as_str(), r.domain.as_str()))
            == Some(&r.counter_id);
        let legacy_head = if checkpointed && previous_head.is_none() && first_segment {
            old.get(&legacy_key)
        } else {
            None
        };
        let migrating = legacy_head.is_some();
        let (old_up, old_down) = previous_head.or(legacy_head).copied().unwrap_or_default();
        let (mut up, mut down) = (r.uplink_bytes as i64, r.downlink_bytes as i64);
        if checkpointed {
            // Spool retries and coalescing can deliver older directional snapshots. A
            // segment never resets: retain each direction's greatest committed value.
            up = up.max(old_up);
            down = down.max(old_down);
        } else if up < old_up || down < old_down {
            limited = true;
            continue;
        }
        if up == old_up && down == old_down && !migrating {
            continue;
        }
        let new_head = checkpointed && previous_head.is_none();
        if new_head
            && !migrating
            && head_counts.get(&r.bucket).copied().unwrap_or(0) >= DETAIL_HISTORY_MAX_ROWS
        {
            limited = true;
            continue;
        }
        let key = (r.bucket, tenant.clone(), user.clone(), r.domain.clone());
        if !persisted_keys.contains(&key) {
            let count = bucket_counts.entry(r.bucket).or_default();
            // Resident capacity is unrelated to the number of distinct names seen over an
            // hour. Keep a separate defensive history budget for high-cardinality input.
            let history_capacity = if checkpointed {
                DETAIL_HISTORY_MAX_ROWS
            } else {
                policy.node_capacity as usize
            };
            if *count >= history_capacity {
                limited = true;
                continue;
            }
            *count += 1;
            persisted_keys.insert(key.clone());
        }
        if new_head && !migrating {
            *head_counts.entry(r.bucket).or_default() += 1;
        }
        let t = totals.entry(key).or_default();
        t.0 =
            t.0.checked_add(up - old_up)
                .ok_or_else(|| StoreError::InvalidData("domain aggregation overflow".into()))?;
        t.1 =
            t.1.checked_add(down - old_down)
                .ok_or_else(|| StoreError::InvalidData("domain aggregation overflow".into()))?;
        buckets.push(r.bucket);
        labels.push(r.label.as_str());
        domains.push(r.domain.as_str());
        epochs.push(head_key.3);
        ups.push(up);
        downs.push(down);
    }
    if !buckets.is_empty() {
        sqlx::query(&kind.sql("INSERT INTO other_domain_counter_heads (node_id,epoch,bucket,label,domain,uplink_bytes,downlink_bytes) SELECT $1,x.* FROM unnest($2::text[],$3::bigint[],$4::text[],$5::text[],$6::bigint[],$7::bigint[]) x(epoch,bucket,label,domain,up,down) ON CONFLICT (node_id,epoch,bucket,label,domain) DO UPDATE SET uplink_bytes=EXCLUDED.uplink_bytes,downlink_bytes=EXCLUDED.downlink_bytes"))
            .bind(node).bind(&epochs).bind(&buckets).bind(&labels).bind(&domains).bind(&ups).bind(&downs).execute(&mut *tx).await?;
        if checkpointed {
            // Transfer each legacy baseline once. New admissions of the same target must
            // start at zero, while an Agent upgrade must not count the old bytes again.
            sqlx::query(&kind.sql("DELETE FROM other_domain_counter_heads h USING unnest($3::bigint[],$4::text[],$5::text[]) x(bucket,label,domain) WHERE h.node_id=$1 AND h.epoch=$2 AND h.bucket=x.bucket AND h.label=x.label AND h.domain=x.domain"))
                .bind(node).bind(&report.epoch).bind(&buckets).bind(&labels).bind(&domains).execute(&mut *tx).await?;
        }
    }
    if !totals.is_empty() {
        let (mut bs, mut ts, mut us, mut ds, mut up, mut down) = (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        for ((b, t, u, d), (a, z)) in &totals {
            bs.push(*b);
            ts.push(t.as_str());
            us.push(u.as_str());
            ds.push(d.as_str());
            up.push(*a);
            down.push(*z);
        }
        sqlx::query(&kind.sql("INSERT INTO user_other_domain_hourly (bucket,node_id,tenant_id,user_id,domain,uplink_bytes,downlink_bytes) SELECT x.bucket,$1,x.tenant,x.user_id,x.domain,x.up,x.down FROM unnest($2::bigint[],$3::text[],$4::text[],$5::text[],$6::bigint[],$7::bigint[]) x(bucket,tenant,user_id,domain,up,down) ON CONFLICT (bucket,node_id,tenant_id,user_id,domain) DO UPDATE SET uplink_bytes=user_other_domain_hourly.uplink_bytes+EXCLUDED.uplink_bytes,downlink_bytes=user_other_domain_hourly.downlink_bytes+EXCLUDED.downlink_bytes"))
            .bind(node).bind(&bs).bind(&ts).bind(&us).bind(&ds).bind(&up).bind(&down).execute(&mut *tx).await?;
    }
    sqlx::query(&kind.sql("INSERT INTO node_other_domain_state (node_id,epoch,read_at,observed_since,limited) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (node_id) DO UPDATE SET epoch=CASE WHEN EXCLUDED.read_at>=node_other_domain_state.read_at THEN EXCLUDED.epoch ELSE node_other_domain_state.epoch END,read_at=GREATEST(node_other_domain_state.read_at,EXCLUDED.read_at),observed_since=CASE WHEN EXCLUDED.read_at>=node_other_domain_state.read_at THEN EXCLUDED.observed_since ELSE node_other_domain_state.observed_since END,limited=CASE WHEN EXCLUDED.read_at>=node_other_domain_state.read_at THEN EXCLUDED.limited ELSE node_other_domain_state.limited OR EXCLUDED.limited END"))
        .bind(node).bind(&report.epoch).bind(report.read_at_unix_secs).bind(since).bind(limited).execute(&mut *tx).await?;
    // Trim only this node's heads, independently of the fleet's daily housekeeping clock.
    sqlx::query(
        &kind.sql("DELETE FROM other_domain_counter_heads WHERE node_id=$1 AND bucket < $2"),
    )
    .bind(node)
    .bind(replay_floor)
    .execute(&mut *tx)
    .await?;
    sqlx::query(&kind.sql("DELETE FROM user_other_domain_hourly WHERE ctid IN (SELECT ctid FROM user_other_domain_hourly WHERE node_id=$1 AND bucket < $2 ORDER BY bucket LIMIT 1024)"))
        .bind(node).bind(now - i64::from(history_hours) * 3600).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn query(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    period: SiteUsagePeriod,
) -> Result<UserOtherDomains> {
    query_for(
        pool,
        actor,
        DetailScope::User { tenant, user },
        period,
        DetailKind::Domain,
        None,
    )
    .await
}

pub(crate) async fn query_node(
    pool: &PgPool,
    actor: &AdminContext,
    node: &str,
    period: SiteUsagePeriod,
) -> Result<UserOtherDomains> {
    query_node_range(pool, actor, node, period, None).await
}

pub(crate) async fn query_node_range(
    pool: &PgPool,
    actor: &AdminContext,
    node: &str,
    period: SiteUsagePeriod,
    range: Option<brocade_deployment::source_traffic::TrafficTimeRange>,
) -> Result<UserOtherDomains> {
    query_for(
        pool,
        actor,
        DetailScope::Node(node),
        period,
        DetailKind::Domain,
        range,
    )
    .await
}

/// Whose detail rows a query reads.
#[derive(Clone, Copy)]
enum DetailScope<'a> {
    /// One user on every ingress machine that carries the user.
    User { tenant: &'a str, user: &'a str },
    /// Every user in the actor's tenant scope on one ingress machine.
    Node(&'a str),
}

async fn query_for(
    pool: &PgPool,
    actor: &AdminContext,
    scope: DetailScope<'_>,
    period: SiteUsagePeriod,
    kind: DetailKind,
    range: Option<brocade_deployment::source_traffic::TrafficTimeRange>,
) -> Result<UserOtherDomains> {
    let nodes: Vec<_> = match scope {
        DetailScope::User { tenant, user } => {
            crate::usage::user_realtime_targets(pool, actor, tenant, user)
                .await?
                .into_iter()
                .filter(|t| !t.labels.is_empty())
                .map(|t| t.node_id)
                .collect()
        }
        DetailScope::Node(node) => crate::site_usage::classified_node(pool, actor, node).await?,
    };
    let bounds = crate::site_usage::destination_bounds(pool, period, range).await?;
    let now = bounds.now;
    let history_hours = crate::retention::ttl_hours(pool, kind.history_table()).await?;
    // Match cleanup's age cutoff, rounding up to the first retained whole-hour bucket.
    let retained_since = (bounds.clock - i64::from(history_hours) * 3600 + 3599) / 3600 * 3600;
    let start = bounds.start.max(retained_since).min(now);
    let policy = policy_for(pool, kind).await?;
    let state=sqlx::query(&kind.sql("SELECT count(*) FILTER (WHERE read_at >= $2)::bigint AS reporting,bool_or(limited) AS limited,max(observed_since) AS since,max(read_at) AS latest FROM node_other_domain_state WHERE node_id=ANY($1)")).bind(&nodes).bind(bounds.clock-600).fetch_one(pool).await?;
    // The window aggregate runs before LIMIT, so an older low-volume host still establishes
    // the first real record. Reuse the exact row filters, including tenant scope and retention.
    let rows = match scope {
        DetailScope::User { tenant, user } => sqlx::query(&kind.sql("SELECT domain,sum(uplink_bytes)::bigint AS up,sum(downlink_bytes)::bigint AS down,min(min(bucket)) OVER () AS first_record FROM user_other_domain_hourly WHERE tenant_id=$1 AND user_id=$2 AND bucket >= $3 AND bucket <= $4 GROUP BY domain ORDER BY sum(uplink_bytes::numeric+downlink_bytes::numeric) DESC,domain LIMIT $5"))
            .bind(tenant).bind(user).bind(start).bind(if range.is_some() { now - 1 } else { now }).bind(i64::from(policy.top_n)).fetch_all(pool).await?,
        DetailScope::Node(node) => sqlx::query(&kind.sql("SELECT domain,sum(uplink_bytes)::bigint AS up,sum(downlink_bytes)::bigint AS down,min(min(bucket)) OVER () AS first_record FROM user_other_domain_hourly WHERE node_id=$1 AND bucket >= $2 AND bucket <= $3 AND ($5::text IS NULL OR tenant_id=$5 OR tenant_id LIKE $6 ESCAPE '\\') GROUP BY domain ORDER BY sum(uplink_bytes::numeric+downlink_bytes::numeric) DESC,domain LIMIT $4"))
            .bind(node).bind(start).bind(if range.is_some() { now - 1 } else { now }).bind(i64::from(policy.top_n)).bind(actor.tenant_scope()).bind(actor.tenant_scope_like_pattern()).fetch_all(pool).await?,
    };
    Ok(UserOtherDomains {
        collection_enabled: policy.enabled,
        top_n: policy.top_n,
        range_start_unix_secs: start,
        range_end_unix_secs: now,
        reporting_nodes: state.get::<i64, _>("reporting") as u64,
        expected_nodes: nodes.len() as u64,
        limited: state.get::<Option<bool>, _>("limited").unwrap_or(false),
        observed_since_unix_secs: state.get("since"),
        first_record_unix_secs: rows.first().map(|row| row.get("first_record")),
        latest_report_unix_secs: state.get("latest"),
        rows: rows
            .into_iter()
            .map(|r| UserOtherDomainRow {
                domain: r.get(kind.key_column()),
                uplink_bytes: r.get::<i64, _>("up") as u64,
                downlink_bytes: r.get::<i64, _>("down") as u64,
            })
            .collect(),
    })
}
pub(crate) async fn prune(pool: &PgPool) -> Result<()> {
    prune_hours(
        pool,
        crate::retention::ttl_hours(pool, DetailKind::Domain.history_table()).await?,
        crate::retention::ttl_hours(pool, DetailKind::FailedTarget.history_table()).await?,
        crate::retention::ttl_hours(pool, "other_domain_counter_heads").await?,
        crate::retention::ttl_hours(pool, "sniffing_failed_counter_heads").await?,
    )
    .await
}

pub(crate) async fn prune_hours(
    pool: &PgPool,
    domain_history_hours: u32,
    failed_history_hours: u32,
    domain_head_hours: u32,
    failed_head_hours: u32,
) -> Result<()> {
    prune_for_hours(
        pool,
        DetailKind::Domain,
        domain_history_hours,
        domain_head_hours,
    )
    .await?;
    prune_for_hours(
        pool,
        DetailKind::FailedTarget,
        failed_history_hours,
        failed_head_hours,
    )
    .await?;
    Ok(())
}

async fn prune_for_hours(
    pool: &PgPool,
    kind: DetailKind,
    history_hours: u32,
    head_hours: u32,
) -> Result<()> {
    let heads = match kind {
        DetailKind::Domain => "other_domain_counter_heads",
        DetailKind::FailedTarget => "sniffing_failed_counter_heads",
    };
    for (table, hours) in [(kind.history_table(), history_hours), (heads, head_hours)] {
        for _ in 0..64 {
            if crate::retention::cleanup::prune_batch(pool, table, hours).await? < 10_000 {
                break;
            }
        }
    }
    Ok(())
}

pub(crate) async fn failed_policy(pool: &PgPool) -> Result<SniffingFailedPolicy> {
    Ok(SniffingFailedPolicy::from_budget(
        policy_for(pool, DetailKind::FailedTarget).await?,
    ))
}
pub(crate) async fn update_failed_policy(
    pool: &PgPool,
    actor: &AdminContext,
    value: SniffingFailedPolicy,
) -> Result<SniffingFailedPolicy> {
    Ok(SniffingFailedPolicy::from_budget(
        update_policy_for(pool, actor, value.budget(), DetailKind::FailedTarget).await?,
    ))
}
pub(crate) async fn record_failed(
    pool: &PgPool,
    node: &str,
    report: SniffingFailedReport,
) -> Result<()> {
    record_for(
        pool,
        node,
        report.map_counters(|r| brocade_deployment::other_domain::OtherDomainCounter {
            counter_id: r.counter_id,
            bucket: r.bucket,
            label: r.label,
            domain: r.target,
            uplink_bytes: r.uplink_bytes,
            downlink_bytes: r.downlink_bytes,
        }),
        DetailKind::FailedTarget,
    )
    .await
}
pub(crate) async fn query_failed(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    period: SiteUsagePeriod,
) -> Result<UserSniffingFailedTargets> {
    failed_targets(
        pool,
        actor,
        DetailScope::User { tenant, user },
        period,
        None,
    )
    .await
}

pub(crate) async fn query_failed_node(
    pool: &PgPool,
    actor: &AdminContext,
    node: &str,
    period: SiteUsagePeriod,
) -> Result<UserSniffingFailedTargets> {
    query_failed_node_range(pool, actor, node, period, None).await
}

pub(crate) async fn query_failed_node_range(
    pool: &PgPool,
    actor: &AdminContext,
    node: &str,
    period: SiteUsagePeriod,
    range: Option<brocade_deployment::source_traffic::TrafficTimeRange>,
) -> Result<UserSniffingFailedTargets> {
    failed_targets(pool, actor, DetailScope::Node(node), period, range).await
}

async fn failed_targets(
    pool: &PgPool,
    actor: &AdminContext,
    scope: DetailScope<'_>,
    period: SiteUsagePeriod,
    range: Option<brocade_deployment::source_traffic::TrafficTimeRange>,
) -> Result<UserSniffingFailedTargets> {
    Ok(
        query_for(pool, actor, scope, period, DetailKind::FailedTarget, range)
            .await?
            .map_rows(|r| UserSniffingFailedRow {
                target: r.domain,
                uplink_bytes: r.uplink_bytes,
                downlink_bytes: r.downlink_bytes,
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_over_budget_and_sensitive_destinations() {
        let mut report = OtherDomainReport {
            version: 1,
            epoch: "0123456789abcdef0123456789abcdef".into(),
            activation_unix_secs: 7201,
            read_at_unix_secs: 7300,
            usage_generation_id: 1,
            limited: false,
            counters: vec![],
        };
        assert!(validate(&report, 7300).is_ok());
        report
            .counters
            .push(brocade_deployment::other_domain::OtherDomainCounter {
                counter_id: 0,
                bucket: 7200,
                label: "a@t#i".into(),
                domain: "https://secret.example/path".into(),
                uplink_bytes: 1,
                downlink_bytes: 0,
            });
        assert!(validate(&report, 7300).is_err());
    }
}
