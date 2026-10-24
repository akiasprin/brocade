//! The same bounded deletion path serves maintenance and the older domain-level prune APIs.
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::{definition, Definition, RetentionCleanupResult, RetentionSchedule};
use crate::{Result, StoreError};

enum Clock {
    Timestamp,
    UnixSeconds,
    UtcDate,
}

struct Specification {
    column: &'static str,
    clock: Clock,
    batch_rows: i64,
    filter: &'static str,
}

fn specification(item: &Definition) -> Specification {
    let (column, clock) = match item.table_name {
        "source_traffic_windows"
        | "source_traffic_daily"
        | "user_site_usage_hourly"
        | "user_other_domain_hourly"
        | "user_sniffing_failed_hourly"
        | "other_domain_counter_heads"
        | "sniffing_failed_counter_heads" => ("bucket", Clock::UnixSeconds),
        "node_load_samples"
        | "node_hop_link_samples"
        | "usage_samples"
        | "usage_chain_samples"
        | "usage_node_windows"
        | "forwarding_usage_windows" => ("window_end", Clock::Timestamp),
        "node_ping_probe_samples" | "e2e_probe_samples" => ("probed_at", Clock::Timestamp),
        "usage_readings" | "forwarding_usage_samples" => ("read_at", Clock::Timestamp),
        "vpngate_candidate_probe_samples"
        | "vpngate_probe_samples"
        | "usage_report_receipts"
        | "forwarding_usage_report_receipts" => ("received_at", Clock::Timestamp),
        "vpngate_server_observations" => ("observed_at", Clock::Timestamp),
        "external_outbound_probe_runs" | "certificate_scan_runs" => {
            ("finished_at", Clock::Timestamp)
        }
        "node_public_ip_events" => ("recorded_at", Clock::Timestamp),
        "machine_events" => ("created_at", Clock::Timestamp),
        "user_online_sources" => ("last_observed_at", Clock::Timestamp),
        "node_traffic_forecasts" => ("day", Clock::UtcDate),
        _ => unreachable!("every compiled retention definition has an expiry specification"),
    };
    let batch_rows = match item.table_name {
        "usage_readings"
        | "usage_samples"
        | "usage_chain_samples"
        | "usage_node_windows"
        | "vpngate_candidate_probe_samples"
        | "vpngate_server_observations" => 50_000,
        "certificate_scan_runs" => 1_000,
        _ => 10_000,
    };
    Specification {
        column,
        clock,
        batch_rows,
        filter: if item.table_name == "certificate_scan_runs" {
            " AND status IN ('succeeded', 'failed')"
        } else {
            ""
        },
    }
}

fn predicate(spec: &Specification) -> String {
    let cutoff = match spec.clock {
        Clock::Timestamp => "now() - make_interval(secs => $1::double precision)",
        Clock::UnixSeconds => "extract(epoch FROM now())::bigint - ($1::double precision)::bigint",
        Clock::UtcDate => {
            "((now() - make_interval(secs => $1::double precision)) AT TIME ZONE 'UTC')::date"
        }
    };
    format!("{} < {cutoff}{}", spec.column, spec.filter)
}

async fn limits(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query("SET LOCAL lock_timeout = '1s'")
        .execute(&mut **tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '10s'")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn delete_batch(
    tx: &mut Transaction<'_, Postgres>,
    item: &Definition,
    hours: u32,
) -> Result<u64> {
    if item.table_name == "node_load_samples" {
        // Compaction must not read raw rows that retention deletes before replacement inserts.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(crate::load_rollup::RETENTION_LOCK)
            .execute(&mut **tx)
            .await?;
    }
    let spec = specification(item);
    let observed = item.table_name == "vpngate_server_observations";
    let returning = if observed {
        "country_code <> 'ZZ' AS counted"
    } else {
        "false AS counted"
    };
    // These histories have BRIN expiry indexes. A global sort of the expired population
    // would defeat bounded catch-up; any expired batch is sufficient for diagnostic history.
    let order = if item.table_name.starts_with("vpngate_") {
        String::new()
    } else {
        format!(" ORDER BY {}", spec.column)
    };
    // Every identifier and SQL fragment is compiled here or in the catalogue. Runtime values
    // are bound; an API-supplied name can only select an existing Definition.
    let query = format!(
        "WITH expired AS MATERIALIZED (
             SELECT ctid FROM {} WHERE {}{order} LIMIT $2 FOR UPDATE SKIP LOCKED
         ), deleted AS (
             DELETE FROM {} history USING expired WHERE history.ctid = expired.ctid
             RETURNING {returning}
         ) SELECT count(*)::bigint AS deleted, count(*) FILTER (WHERE counted)::bigint AS counted FROM deleted",
        item.table_name, predicate(&spec), item.table_name
    );
    let row = sqlx::query(&query)
        .bind(f64::from(hours) * 3600.0)
        .bind(spec.batch_rows)
        .fetch_one(&mut **tx)
        .await?;
    let deleted: i64 = row.try_get("deleted")?;
    if observed {
        let counted: i64 = row.try_get("counted")?;
        sqlx::query("UPDATE vpngate_catalog_state SET retained_observation_count = greatest(retained_observation_count - $1, 0) WHERE id = TRUE")
            .bind(counted).execute(&mut **tx).await?;
    }
    Ok(u64::try_from(deleted).expect("SQL count is nonnegative"))
}

pub(crate) async fn prune_batch(pool: &PgPool, table: &str, hours: u32) -> Result<u64> {
    let item = definition(table)
        .ok_or_else(|| StoreError::NotFound(format!("retention policy {table}")))?;
    // The older explicit load-prune API uses zero to expire all windows. Runtime policies
    // still enforce their one-hour minimum before reaching run_one.
    let minimum = if matches!(table, "node_load_samples" | "node_hop_link_samples") {
        0
    } else {
        item.min_ttl_hours
    };
    let hours = hours.clamp(minimum, super::MAX_RETENTION_HOURS);
    let mut tx = pool.begin().await?;
    limits(&mut tx).await?;
    let rows = delete_batch(&mut tx, item, hours).await?;
    tx.commit().await?;
    Ok(rows)
}

fn error_code(error: &StoreError) -> &'static str {
    match error {
        StoreError::Sqlx(sqlx::Error::Database(error)) => match error.code().as_deref() {
            Some("57014") => "statement_timeout",
            Some("55P03") => "lock_timeout",
            _ => "database_error",
        },
        StoreError::Sqlx(sqlx::Error::PoolTimedOut) => "connection_timeout",
        _ => "cleanup_error",
    }
}

pub(super) async fn run_one(
    pool: &PgPool,
    item: &Definition,
    schedule: &RetentionSchedule,
) -> Result<Option<RetentionCleanupResult>> {
    let hours = schedule.ttl_hours(item.table_name);
    let mut tx = pool.begin().await?;
    limits(&mut tx).await?;
    // The transaction lock covers due checking, deletion and status publication. Another
    // Console skips this table instead of duplicating work or overwriting a newer result.
    let claimed: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 824519))")
            .bind(item.table_name)
            .fetch_one(&mut *tx)
            .await?;
    if !claimed {
        return Ok(None);
    }
    let due: bool = sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM data_retention_cleanup WHERE table_name = $1 AND ttl_hours = $2 AND next_run_at > now())")
        .bind(item.table_name).bind(hours as i32).fetch_one(&mut *tx).await?;
    if !due {
        return Ok(None);
    }
    sqlx::query("SAVEPOINT retention_delete")
        .execute(&mut *tx)
        .await?;
    let deleted = delete_batch(&mut tx, item, hours).await;
    let (rows, failure, backlog) = match deleted {
        Ok(rows) => {
            sqlx::query("SAVEPOINT retention_probe")
                .execute(&mut *tx)
                .await?;
            sqlx::query("SET LOCAL statement_timeout = '1s'")
                .execute(&mut *tx)
                .await?;
            let query = format!(
                "SELECT EXISTS (SELECT 1 FROM {} WHERE {})",
                item.table_name,
                predicate(&specification(item))
            );
            let probe = sqlx::query_scalar::<_, bool>(&query)
                .bind(f64::from(hours) * 3600.0)
                .fetch_one(&mut *tx)
                .await;
            let backlog = match probe {
                Ok(value) => Some(value),
                Err(_) => {
                    // A slow diagnostic must not undo a successful bounded DELETE. Unknown
                    // backlog is retried soon and shown explicitly instead of claiming zero.
                    sqlx::query("ROLLBACK TO SAVEPOINT retention_probe")
                        .execute(&mut *tx)
                        .await?;
                    None
                }
            };
            (rows, None, backlog)
        }
        Err(error) => {
            sqlx::query("ROLLBACK TO SAVEPOINT retention_delete")
                .execute(&mut *tx)
                .await?;
            (0, Some(error_code(&error)), None)
        }
    };
    sqlx::query("SET LOCAL statement_timeout = '10s'")
        .execute(&mut *tx)
        .await?;
    let retry_secs =
        if failure.is_some() || backlog != Some(false) || item.table_name == "usage_readings" {
            60
        } else {
            3600
        };
    sqlx::query(
        "INSERT INTO data_retention_cleanup (table_name, ttl_hours, finished_at, last_success_at, deleted_rows, error_code, has_backlog, next_run_at)
         VALUES ($1, $2, clock_timestamp(), CASE WHEN $4::text IS NULL THEN clock_timestamp() END, $3, $4, $5, clock_timestamp() + make_interval(secs => $6))
         ON CONFLICT (table_name) DO UPDATE SET ttl_hours = EXCLUDED.ttl_hours, finished_at = EXCLUDED.finished_at,
             last_success_at = COALESCE(EXCLUDED.last_success_at, data_retention_cleanup.last_success_at),
             deleted_rows = CASE WHEN EXCLUDED.error_code IS NULL THEN EXCLUDED.deleted_rows ELSE data_retention_cleanup.deleted_rows END,
             error_code = EXCLUDED.error_code, has_backlog = EXCLUDED.has_backlog, next_run_at = EXCLUDED.next_run_at"
    ).bind(item.table_name).bind(hours as i32).bind(rows as i64).bind(failure).bind(backlog).bind(f64::from(retry_secs))
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(RetentionCleanupResult {
        table_name: item.table_name.to_owned(),
        deleted_rows: rows,
        error_code: failure.map(str::to_owned),
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn all_catalogued_tables_have_expiry_specs() {
        for item in super::super::DEFINITIONS {
            let spec = super::specification(item);
            assert!(spec.batch_rows > 0);
            assert!(!super::predicate(&spec).is_empty());
        }
    }
}
