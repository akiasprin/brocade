-- Read-only relation/query baseline (temporary tables only); run on an isolated restored backup.
-- Emits no node IDs, targets, raw rows or query plans. No cache drop/restart: first is not "cold".
\set ON_ERROR_STOP on
SET statement_timeout = '60s';
SET jit = off;
CREATE TEMP TABLE ping_benchmark_results (result jsonb);
DO $$
DECLARE
    normalized boolean := to_regclass('node_ping_probe_series') IS NOT NULL;
    labels text;
    detail text;
    latest text;
    plan jsonb;
    samples jsonb := '[]';
    query_text text;
    query_name text;
    run integer;
    relation_bytes bigint;
    point_count bigint;
BEGIN
    IF normalized THEN
        labels := 'SELECT node_id,target,family,id FROM node_ping_probe_series';
        detail := 'SELECT target,family,probed_at,attempted,latency_us,skip_reason
          FROM node_ping_probe_series series JOIN node_ping_probe_samples sample ON sample.series_id=series.id
          WHERE series.node_id=(SELECT node_id FROM bench_series ORDER BY node_id LIMIT 1)
            AND probed_at >= (SELECT max(probed_at) FROM node_ping_probe_samples) - interval ''24 hours''
          ORDER BY probed_at';
        latest := 'SELECT series.node_id,series.target,series.family,latest.* FROM node_ping_probe_series series
          JOIN LATERAL (SELECT probed_at,attempted,latency_us,skip_reason FROM node_ping_probe_samples sample
            WHERE sample.series_id=series.id ORDER BY probed_at DESC LIMIT 1) latest ON TRUE';
    ELSE
        labels := 'SELECT DISTINCT node_id,target,family FROM node_ping_probe_samples';
        detail := 'SELECT target,family,probed_at,attempted,latency_us,skip_reason FROM node_ping_probe_samples
          WHERE node_id=(SELECT node_id FROM bench_series ORDER BY node_id LIMIT 1)
            AND probed_at >= (SELECT max(probed_at) FROM node_ping_probe_samples) - interval ''24 hours''
          ORDER BY probed_at';
        latest := 'SELECT series.node_id,series.target,series.family,latest.* FROM bench_series series
          JOIN LATERAL (SELECT probed_at,attempted,latency_us,skip_reason FROM node_ping_probe_samples sample
            WHERE sample.node_id=series.node_id AND sample.target=series.target AND sample.family=series.family
            ORDER BY probed_at DESC LIMIT 1) latest ON TRUE';
    END IF;
    EXECUTE 'CREATE TEMP TABLE bench_series AS ' || labels;
    ANALYZE bench_series;
    SELECT count(*) INTO point_count FROM node_ping_probe_samples;
    relation_bytes := pg_total_relation_size('node_ping_probe_samples');
    IF normalized THEN relation_bytes := relation_bytes + pg_total_relation_size('node_ping_probe_series'); END IF;
    FOREACH query_name IN ARRAY ARRAY['detail_24h','latest_fleet'] LOOP
        query_text := CASE query_name WHEN 'detail_24h' THEN detail ELSE latest END;
        FOR run IN 1..6 LOOP
            EXECUTE 'EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) ' || query_text INTO plan;
            samples := samples || jsonb_build_object('query',query_name,'run',run,
                'execution_ms',plan->0->'Execution Time','planning_ms',plan->0->'Planning Time',
                'rows',plan->0->'Plan'->'Actual Rows',
                'shared_hit_blocks',plan->0->'Plan'->'Shared Hit Blocks',
                'shared_read_blocks',plan->0->'Plan'->'Shared Read Blocks');
        END LOOP;
    END LOOP;
    INSERT INTO ping_benchmark_results VALUES (jsonb_build_object(
        'normalized',normalized,'points',point_count,'series',(SELECT count(*) FROM bench_series),
        'total_relation_bytes',relation_bytes,'sample_heap_bytes',pg_relation_size('node_ping_probe_samples'),
        'sample_index_bytes',pg_indexes_size('node_ping_probe_samples'),'runs',samples));
END $$;
SELECT jsonb_pretty(result) FROM ping_benchmark_results;
