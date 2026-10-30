//! Optional source observations, independent of billing locks, baselines and receipts.
use crate::{AdminContext, NodeInflowRow, NodeInflowSource, NodeInflowUsage, Result, StoreError};
use brocade_deployment::source_traffic::{
    valid_source_ip, SourceTrafficReport, TrafficTimeRange, SOURCE_TRAFFIC_MAX_ROWS,
};
use serde::Serialize;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub(crate) struct SourceOwner {
    pub tenant_id: String,
    pub user_id: String,
    pub app_id: String,
    pub chain_id: String,
    pub ingress_id: String,
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData(message.into())
}

fn add_bytes(total: &mut u64, value: u64) -> Result<()> {
    *total = total
        .checked_add(value)
        .ok_or_else(|| invalid("source observation total overflow"))?;
    Ok(())
}

fn validate(report: &SourceTrafficReport, now: i64) -> Result<()> {
    if report.version != 1
        || report.epoch.len() != 32
        || !report.epoch.bytes().all(|b| b.is_ascii_hexdigit())
        || report.counters.len() > SOURCE_TRAFFIC_MAX_ROWS
        || report.activation_unix_secs <= 0
        || report.activation_unix_secs > report.read_at_unix_secs
        || report.read_at_unix_secs > now + 60
        || report.read_at_unix_secs < now - 600
        || report.usage_generation_id <= 0
    {
        return Err(invalid("invalid source observation envelope"));
    }
    let bucket = report.read_at_unix_secs / 30 * 30;
    let mut seen = BTreeSet::new();
    for row in &report.counters {
        if ![bucket, bucket - 30].contains(&row.bucket)
            || row.label.is_empty()
            || row.label.len() > 256
            || !valid_source_ip(&row.source_ip)
            || row.uplink_bytes > i64::MAX as u64
            || row.downlink_bytes > i64::MAX as u64
            || !seen.insert((row.bucket, &row.label, &row.source_ip))
        {
            return Err(invalid("invalid or duplicate source observation row"));
        }
    }
    Ok(())
}

pub(crate) async fn record(pool: &PgPool, node: &str, report: SourceTrafficReport) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *tx)
        .await?;
    let locked: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(1481785692,hashtext($1))")
            .bind(node)
            .fetch_one(&mut *tx)
            .await?;
    if !locked {
        return Err(invalid("source observation busy; retry later"));
    }
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(&mut *tx)
        .await?;
    validate(&report, now)?;
    let previous = sqlx::query("SELECT epoch,read_at,observed_since,limited FROM node_source_traffic_state WHERE node_id=$1")
        .bind(node).fetch_optional(&mut *tx).await?;
    if previous
        .as_ref()
        .is_some_and(|p| p.get::<i64, _>("read_at") >= report.read_at_unix_secs)
    {
        tx.commit().await?;
        return Ok(());
    }
    let same_epoch = previous
        .as_ref()
        .is_some_and(|p| p.get::<String, _>("epoch") == report.epoch);
    if previous.as_ref().is_some_and(|p| {
        !same_epoch && p.get::<i64, _>("observed_since") >= report.activation_unix_secs
    }) {
        return Err(invalid("stale source observation epoch"));
    }
    let owners =
        crate::usage::source_traffic_owners(&mut tx, node, report.usage_generation_id).await?;
    let floor = report.read_at_unix_secs / 30 * 30 - 30;
    let old = sqlx::query("SELECT bucket,label,host(source_ip) AS ip,tenant_id,user_id,app_id,chain_id,ingress_id,uplink_bytes,downlink_bytes FROM source_traffic_windows WHERE node_id=$1 AND epoch=$2 AND bucket >= $3")
        .bind(node).bind(&report.epoch).bind(floor).fetch_all(&mut *tx).await?
        .into_iter().map(|r| ((r.get::<i64,_>("bucket"),r.get::<String,_>("label"),r.get::<String,_>("ip")),r)).collect::<BTreeMap<_,_>>();
    // Epoch rotation must not evade the per-window database budget.
    let mut counts = sqlx::query("SELECT bucket,count(*)::bigint AS n FROM source_traffic_windows WHERE node_id=$1 AND bucket >= $2 GROUP BY bucket")
        .bind(node).bind(floor).fetch_all(&mut *tx).await?.into_iter()
        .map(|r|(r.get::<i64,_>("bucket"),r.get::<i64,_>("n"))).collect::<BTreeMap<_,_>>();
    let mut limited = report.limited
        || previous.as_ref().is_some_and(|p| {
            same_epoch
                && (p.get::<bool, _>("limited")
                    || report.read_at_unix_secs - p.get::<i64, _>("read_at") > 30)
        });
    let first_available = report.activation_unix_secs.max(floor);
    let since = if same_epoch {
        previous
            .as_ref()
            .map(|p| p.get("observed_since"))
            .unwrap_or(first_available)
    } else {
        first_available
    };
    let (
        mut bs,
        mut ls,
        mut ips,
        mut ts,
        mut us,
        mut apps,
        mut chains,
        mut ingresses,
        mut ups,
        mut downs,
        mut delta_up,
        mut delta_down,
    ) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    for row in &report.counters {
        let Some(owner) = owners.get(&row.label) else {
            limited = true;
            continue;
        };
        let old_row = old.get(&(row.bucket, row.label.clone(), row.source_ip.clone()));
        let old_up = old_row.map_or(0, |r| r.get::<i64, _>("uplink_bytes"));
        let old_down = old_row.map_or(0, |r| r.get::<i64, _>("downlink_bytes"));
        let up = row.uplink_bytes as i64;
        let down = row.downlink_bytes as i64;
        if up < old_up || down < old_down {
            limited = true;
            continue;
        }
        if up == old_up && down == old_down {
            continue;
        }
        if old_row.is_none() {
            let count = counts.entry(row.bucket).or_default();
            if *count >= 4096 {
                limited = true;
                continue;
            }
            *count += 1;
        }
        // Identity is frozen when the counter is first observed, never joined to today's model.
        let owner = old_row
            .map(|r| SourceOwner {
                tenant_id: r.get("tenant_id"),
                user_id: r.get("user_id"),
                app_id: r.get("app_id"),
                chain_id: r.get("chain_id"),
                ingress_id: r.get("ingress_id"),
            })
            .unwrap_or_else(|| owner.clone());
        bs.push(row.bucket);
        ls.push(row.label.as_str());
        ips.push(row.source_ip.as_str());
        ts.push(owner.tenant_id);
        us.push(owner.user_id);
        apps.push(owner.app_id);
        chains.push(owner.chain_id);
        ingresses.push(owner.ingress_id);
        ups.push(up);
        downs.push(down);
        delta_up.push(up - old_up);
        delta_down.push(down - old_down);
    }
    if !bs.is_empty() {
        sqlx::query("INSERT INTO source_traffic_windows (node_id,epoch,bucket,label,source_ip,tenant_id,user_id,app_id,chain_id,ingress_id,uplink_bytes,downlink_bytes)
            SELECT $1,$2,x.b,x.l,x.ip::inet,x.t,x.u,x.a,x.c,x.i,x.up,x.down FROM unnest($3::bigint[],$4::text[],$5::text[],$6::text[],$7::text[],$8::text[],$9::text[],$10::text[],$11::bigint[],$12::bigint[]) x(b,l,ip,t,u,a,c,i,up,down)
            ON CONFLICT (node_id,epoch,bucket,label,source_ip) DO UPDATE SET uplink_bytes=EXCLUDED.uplink_bytes,downlink_bytes=EXCLUDED.downlink_bytes")
            .bind(node).bind(&report.epoch).bind(&bs).bind(&ls).bind(&ips).bind(&ts).bind(&us).bind(&apps).bind(&chains).bind(&ingresses).bind(&ups).bind(&downs).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO source_traffic_daily (node_id,bucket,tenant_id,user_id,app_id,chain_id,ingress_id,source_ip,uplink_bytes,downlink_bytes)
            SELECT $1,(x.b+28800)/86400*86400-28800,x.t,x.u,x.a,x.c,x.i,x.ip::inet,sum(x.up),sum(x.down)
            FROM unnest($2::bigint[],$3::text[],$4::text[],$5::text[],$6::text[],$7::text[],$8::text[],$9::bigint[],$10::bigint[]) x(b,t,u,a,c,i,ip,up,down)
            GROUP BY 2,x.t,x.u,x.a,x.c,x.i,x.ip
            ON CONFLICT (node_id,bucket,tenant_id,user_id,app_id,chain_id,ingress_id,source_ip) DO UPDATE SET uplink_bytes=source_traffic_daily.uplink_bytes+EXCLUDED.uplink_bytes,downlink_bytes=source_traffic_daily.downlink_bytes+EXCLUDED.downlink_bytes")
            .bind(node).bind(&bs).bind(&ts).bind(&us).bind(&apps).bind(&chains).bind(&ingresses).bind(&ips).bind(&delta_up).bind(&delta_down).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO node_source_traffic_state (node_id,epoch,read_at,observed_since,limited) VALUES ($1,$2,$3,$4,$5) ON CONFLICT(node_id) DO UPDATE SET epoch=EXCLUDED.epoch,read_at=EXCLUDED.read_at,observed_since=EXCLUDED.observed_since,limited=EXCLUDED.limited")
        .bind(node).bind(&report.epoch).bind(report.read_at_unix_secs).bind(since).bind(limited).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct NodeInflowDetailRow {
    #[serde(flatten)]
    pub traffic: NodeInflowRow,
    pub source_ip: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NodeInflowDetails {
    pub range_start_unix_secs: i64,
    pub range_end_unix_secs: i64,
    pub observed_since_unix_secs: Option<i64>,
    pub latest_report_unix_secs: Option<i64>,
    pub limited: bool,
    pub accounting_mismatch: bool,
    pub accounting_uplink_bytes: u64,
    pub accounting_downlink_bytes: u64,
    pub rows: Vec<NodeInflowDetailRow>,
}

type SourceKey = (Option<String>, Option<String>, Option<String>);
fn source_key(row: &NodeInflowRow) -> SourceKey {
    (
        if row.chain_id.is_none() {
            row.app_id.clone()
        } else {
            None
        },
        row.chain_id.clone(),
        if row.chain_id.is_none() {
            row.ingress_id.clone()
        } else {
            None
        },
    )
}

pub(crate) async fn details(
    pool: &PgPool,
    actor: &AdminContext,
    node: &str,
    tenant: &str,
    user: &str,
    base: NodeInflowUsage,
    range: Option<TrafficTimeRange>,
) -> Result<NodeInflowDetails> {
    // Validate both user existence/tenant scope and the administrator-only observation boundary.
    crate::usage::user_realtime_targets(pool, actor, tenant, user).await?;
    let state = sqlx::query(
        "SELECT observed_since,read_at,limited FROM node_source_traffic_state WHERE node_id=$1",
    )
    .bind(node)
    .fetch_optional(pool)
    .await?;
    // Older generation bindings lack chain_id. The same day's frozen ingress rollup is the
    // only safe fallback; today's ingress configuration must never rewrite source history.
    let table = if range.is_some() {
        "source_traffic_windows"
    } else {
        "source_traffic_daily"
    };
    let source_sql = format!("SELECT s.app_id,coalesce(nullif(s.chain_id,''),r.chain_id,'') AS chain_id,
        CASE WHEN coalesce(nullif(s.chain_id,''),r.chain_id) IS NULL THEN s.ingress_id END AS ingress_id,
        host(s.source_ip) AS ip,sum(s.uplink_bytes)::bigint AS up,sum(s.downlink_bytes)::bigint AS down
        FROM {table} s LEFT JOIN usage_user_ingress_rollups r ON r.node_id=s.node_id AND r.tenant_id=s.tenant_id
          AND r.user_id=s.user_id AND r.app_id=s.app_id AND r.ingress_id=s.ingress_id
          AND r.period_start=to_timestamp(((s.bucket+28800)/86400*86400-28800)::double precision)
        WHERE s.node_id=$1 AND s.tenant_id=$2 AND s.user_id=$3 AND s.bucket >= $4 AND s.bucket < $5
        GROUP BY 1,2,3,4 ORDER BY sum(s.uplink_bytes::numeric+s.downlink_bytes::numeric) DESC,1,2,3,4 LIMIT 2049");
    let source_rows = sqlx::query(&source_sql)
        .bind(node)
        .bind(tenant)
        .bind(user)
        .bind(base.range_start_unix_secs)
        .bind(base.range_end_unix_secs)
        .fetch_all(pool)
        .await?;
    let mut limited =
        source_rows.len() > 2048 || state.as_ref().is_none_or(|s| s.get::<bool, _>("limited"));
    if range.is_some() {
        let hours = crate::retention::ttl_hours(pool, "source_traffic_windows").await?;
        let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
            .fetch_one(pool)
            .await?;
        limited |= base.range_start_unix_secs < now - i64::from(hours) * 3600;
    }
    let mut rows = Vec::new();
    let mut known = BTreeMap::<SourceKey, (u64, u64)>::new();
    let mut observed_up = 0;
    let mut observed_down = 0;
    for row in source_rows.into_iter().take(2048) {
        let chain: String = row.try_get("chain_id")?;
        let traffic = NodeInflowRow {
            tenant_id: tenant.into(),
            user_id: user.into(),
            source: NodeInflowSource::Chain,
            app_id: Some(row.try_get("app_id")?),
            chain_id: (!chain.is_empty()).then_some(chain),
            ingress_id: row.try_get("ingress_id")?,
            uplink_bytes: row.try_get::<i64, _>("up")? as u64,
            downlink_bytes: row.try_get::<i64, _>("down")? as u64,
            has_gap: false,
        };
        let sum = known.entry(source_key(&traffic)).or_default();
        add_bytes(&mut sum.0, traffic.uplink_bytes)?;
        add_bytes(&mut sum.1, traffic.downlink_bytes)?;
        add_bytes(&mut observed_up, traffic.uplink_bytes)?;
        add_bytes(&mut observed_down, traffic.downlink_bytes)?;
        rows.push(NodeInflowDetailRow {
            traffic,
            source_ip: Some(row.try_get("ip")?),
        });
    }
    let mut accounting_up = 0;
    let mut accounting_down = 0;
    let mut authenticated_up = 0;
    let mut authenticated_down = 0;
    let mut authenticated_gap = false;
    for mut row in base
        .rows
        .into_iter()
        .filter(|r| r.tenant_id == tenant && r.user_id == user)
    {
        add_bytes(&mut accounting_up, row.uplink_bytes)?;
        add_bytes(&mut accounting_down, row.downlink_bytes)?;
        if row.source != NodeInflowSource::Forwarding {
            add_bytes(&mut authenticated_up, row.uplink_bytes)?;
            add_bytes(&mut authenticated_down, row.downlink_bytes)?;
            authenticated_gap |= row.has_gap;
        }
        let (up, down) = if row.source == NodeInflowSource::Chain {
            // One chain can have several historical app rows. Consume its observation only
            // once across those rows before deciding whether anything exceeds the ledger.
            known
                .get_mut(&source_key(&row))
                .map_or((0, 0), |remaining| {
                    let up = remaining.0.min(row.uplink_bytes);
                    let down = remaining.1.min(row.downlink_bytes);
                    remaining.0 -= up;
                    remaining.1 -= down;
                    (up, down)
                })
        } else {
            (0, 0)
        };
        row.uplink_bytes = row.uplink_bytes.saturating_sub(up);
        row.downlink_bytes = row.downlink_bytes.saturating_sub(down);
        if row.uplink_bytes > 0 || row.downlink_bytes > 0 {
            rows.push(NodeInflowDetailRow {
                traffic: row,
                source_ip: None,
            });
        }
    }
    let accounting_mismatch = known.values().any(|&(up, down)| up > 0 || down > 0);
    if accounting_mismatch {
        // A day's billing partition can predate a chain change. Subtract observed bytes
        // once across authenticated traffic instead of duplicating them in another chain's
        // residual. Their remaining chain attribution is unknown; forwarding stays separate.
        rows.retain(|row| {
            row.source_ip.is_some() || row.traffic.source == NodeInflowSource::Forwarding
        });
        let up = authenticated_up.saturating_sub(observed_up);
        let down = authenticated_down.saturating_sub(observed_down);
        if up > 0 || down > 0 {
            rows.push(NodeInflowDetailRow {
                traffic: NodeInflowRow {
                    tenant_id: tenant.into(),
                    user_id: user.into(),
                    source: NodeInflowSource::Unattributed,
                    app_id: None,
                    chain_id: None,
                    ingress_id: None,
                    uplink_bytes: up,
                    downlink_bytes: down,
                    has_gap: authenticated_gap,
                },
                source_ip: None,
            });
        }
    }
    Ok(NodeInflowDetails {
        range_start_unix_secs: base.range_start_unix_secs,
        range_end_unix_secs: base.range_end_unix_secs,
        observed_since_unix_secs: state.as_ref().map(|s| s.get("observed_since")),
        latest_report_unix_secs: state.as_ref().map(|s| s.get("read_at")),
        limited,
        accounting_mismatch,
        accounting_uplink_bytes: accounting_up,
        accounting_downlink_bytes: accounting_down,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use brocade_deployment::source_traffic::SourceTrafficCounter;

    #[test]
    fn validates_source_envelopes_and_rejects_duplicate_counters() {
        let mut report = SourceTrafficReport {
            version: 1,
            epoch: "0123456789abcdef0123456789abcdef".into(),
            activation_unix_secs: 1800000000,
            read_at_unix_secs: 1800000035,
            usage_generation_id: 1,
            limited: false,
            counters: vec![SourceTrafficCounter {
                bucket: 1800000030,
                label: "a@t#i".into(),
                source_ip: "192.0.2.1".into(),
                uplink_bytes: 12,
                downlink_bytes: 34,
            }],
        };
        assert!(validate(&report, 1800000035).is_ok());
        report.counters.push(report.counters[0].clone());
        assert!(validate(&report, 1800000035).is_err());
        report.counters.pop();
        report.counters[0].source_ip = "192.0.2.1/24".into();
        assert!(validate(&report, 1800000035).is_err());
    }

    #[test]
    fn compatibility_schema_matches_the_fresh_install_definition() {
        const BEGIN: &str = "-- BEGIN SOURCE TRAFFIC SCHEMA";
        const END: &str = "-- END SOURCE TRAFFIC SCHEMA";
        fn section(text: &str) -> &str {
            let start = text.find(BEGIN).expect("source schema begin marker exists");
            let end = text.find(END).expect("source schema end marker exists");
            assert!(start < end, "source schema markers are ordered");
            &text[start..end + END.len()]
        }
        assert_eq!(
            section(include_str!("../migrations/0001_init.sql")),
            section(include_str!("../../../scripts/compat-source-traffic.sql"))
        );
    }
}
