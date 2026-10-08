-- Stop all Console writers; back up and rehearse on a restored database first.
-- Only redundant zero-user detail is removed. Nonzero/gap detail and all daily ledgers survive.
-- Create node_schema_file from the target 0001 USAGE NODE WINDOWS SCHEMA block; index_schema_file
-- contains that same 0001's due index and the three revised usage newest-first indexes.
-- psql -X -v app_role=brocade -v node_schema_file=/verified/windows.sql \
--   -v index_schema_file=/verified/indexes.sql -f scripts/compat-usage-sparse.sql
-- No automatic SQLx checksum update, no CASCADE, no old-binary rollback after success.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL ROLE :"app_role";
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '15min';
LOCK TABLE usage_samples, usage_chain_samples IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF to_regclass('usage_node_windows') IS NOT NULL
       OR to_regclass('node_usage_windows') IS NULL THEN
        RAISE EXCEPTION 'usage projection already exists or legacy source is absent; inspect before retry';
    END IF;
END $$;

CREATE TEMP TABLE usage_sparse_before ON COMMIT DROP AS
SELECT count(*) FILTER (WHERE uplink_bytes <> 0 OR downlink_bytes <> 0 OR has_gap) AS retained,
       coalesce(sum(uplink_bytes),0) AS up, coalesce(sum(downlink_bytes),0) AS down,
       count(*) FILTER (WHERE has_gap) AS gaps FROM usage_samples;
CREATE TEMP TABLE usage_sparse_ledger_before ON COMMIT DROP AS SELECT * FROM usage_rollups;
CREATE TEMP TABLE usage_sparse_node_ledger_before ON COMMIT DROP AS SELECT * FROM usage_node_rollups;

\i :node_schema_file

INSERT INTO usage_node_windows
    (node_id, tenant_id, window_end, user_uplink_bytes, user_downlink_bytes,
     relay_uplink_bytes, relay_downlink_bytes)
SELECT node_id, tenant_id, window_end,
       coalesce(sum(uplink_bytes) FILTER (WHERE kind='user'), 0)::bigint,
       coalesce(sum(downlink_bytes) FILTER (WHERE kind='user'), 0)::bigint,
       coalesce(sum(uplink_bytes) FILTER (WHERE kind='relay'), 0)::bigint,
       coalesce(sum(downlink_bytes) FILTER (WHERE kind='relay'), 0)::bigint
FROM node_usage_windows GROUP BY node_id, tenant_id, window_end;

-- Every scope and actual report boundary, including all-zero windows, must remain identical.
DO $$ BEGIN
    IF EXISTS (
        (SELECT node_id, tenant_id, window_end,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind='user'),0)::bigint,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind='user'),0)::bigint,
                coalesce(sum(uplink_bytes) FILTER (WHERE kind='relay'),0)::bigint,
                coalesce(sum(downlink_bytes) FILTER (WHERE kind='relay'),0)::bigint
         FROM node_usage_windows GROUP BY node_id, tenant_id, window_end)
        EXCEPT
        SELECT node_id, tenant_id, window_end, user_uplink_bytes, user_downlink_bytes,
               relay_uplink_bytes, relay_downlink_bytes FROM usage_node_windows
    ) OR (SELECT count(*) FROM usage_node_windows) <>
         (SELECT count(*) FROM (SELECT DISTINCT node_id, tenant_id, window_end FROM node_usage_windows) old)
    THEN RAISE EXCEPTION 'usage chart projection differs from original detail'; END IF;
END $$;
DROP VIEW node_usage_windows;

-- These two old indexes served reads now fulfilled by daily/machine projections, not a new
-- unique or integrity constraint. Newest-first detail queries retain their own targeted paths.
DROP INDEX usage_samples_by_user_app_window;
DROP INDEX usage_chain_samples_by_chain_window;
DROP INDEX usage_samples_by_user_window;
DROP INDEX usage_samples_by_node_window;
DROP INDEX usage_chain_samples_by_node_window;
DROP INDEX vpngate_exit_reputations_due;
\i :index_schema_file

ALTER TABLE usage_samples SET (autovacuum_vacuum_scale_factor=0.02, autovacuum_analyze_scale_factor=0.02);
ALTER TABLE usage_chain_samples SET (autovacuum_vacuum_scale_factor=0.02, autovacuum_analyze_scale_factor=0.02);
ALTER TABLE vpngate_candidate_probe_latest SET (autovacuum_vacuum_scale_factor=0.02, autovacuum_analyze_scale_factor=0.02);
ALTER TABLE vpngate_exit_reputations SET (fillfactor=85, autovacuum_vacuum_scale_factor=0.02, autovacuum_analyze_scale_factor=0.02);

DELETE FROM usage_samples WHERE uplink_bytes=0 AND downlink_bytes=0 AND NOT has_gap;
-- Targeted physical rewrite during the already-stopped maintenance window, not recurring
-- VACUUM FULL. This frees the old zero-row/index files instead of claiming DELETE shrinks them.
-- A heap rewrite here would retain this transaction's freshly deleted tuples for MVCC.
-- Run the companion reclaim script only AFTER this transaction commits, still with writers off.
REINDEX TABLE usage_chain_samples;
REINDEX TABLE vpngate_candidate_probe_latest;

DO $$ BEGIN
    IF EXISTS (
        SELECT count(*), coalesce(sum(uplink_bytes),0), coalesce(sum(downlink_bytes),0), count(*) FILTER (WHERE has_gap)
        FROM usage_samples
        EXCEPT SELECT retained, up, down, gaps FROM usage_sparse_before
    ) OR EXISTS (
        (TABLE usage_rollups EXCEPT TABLE usage_sparse_ledger_before)
        UNION ALL (TABLE usage_sparse_ledger_before EXCEPT TABLE usage_rollups)
    ) OR EXISTS (
        (TABLE usage_node_rollups EXCEPT TABLE usage_sparse_node_ledger_before)
        UNION ALL (TABLE usage_sparse_node_ledger_before EXCEPT TABLE usage_node_rollups)
    ) THEN RAISE EXCEPTION 'usage totals, evidence count or daily ledger changed'; END IF;
END $$;
ANALYZE usage_samples;
ANALYZE usage_chain_samples;
ANALYZE usage_node_windows;
ANALYZE vpngate_exit_reputations;
ANALYZE vpngate_candidate_probe_latest;
COMMIT;
