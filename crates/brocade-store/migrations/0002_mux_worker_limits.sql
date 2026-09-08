-- Match Xray's uint32 worker-count and idle-TTL fields without a separate size/time
-- policy. BIGINT is needed because PostgreSQL INTEGER cannot hold all uint32 values.
-- Existing settings and defaults are preserved.
ALTER TABLE control_state
    DROP CONSTRAINT control_state_relay_mux_ranges;
ALTER TABLE control_state
    ALTER COLUMN relay_mux_min_idle_workers TYPE BIGINT,
    ALTER COLUMN relay_mux_max_idle_workers TYPE BIGINT,
    ALTER COLUMN relay_mux_max_probing_workers TYPE BIGINT,
    ALTER COLUMN relay_mux_idle_ttl_secs TYPE BIGINT;
ALTER TABLE control_state
    ADD CONSTRAINT control_state_relay_mux_ranges CHECK (
        relay_mux_concurrency BETWEEN 1 AND 128
        AND relay_mux_min_idle_workers BETWEEN 0 AND 4294967295
        AND relay_mux_max_idle_workers BETWEEN 1 AND 4294967295
        AND relay_mux_min_idle_workers <= relay_mux_max_idle_workers
        AND relay_mux_max_probing_workers BETWEEN 1 AND relay_mux_max_idle_workers
        AND relay_mux_probe_interval_secs BETWEEN 2 AND 60
        AND relay_mux_probe_timeout_ms BETWEEN 200 AND 10000
        AND relay_mux_probe_timeout_ms < relay_mux_probe_interval_secs * 1000
        AND relay_mux_idle_ttl_secs BETWEEN 1 AND 4294967295
        AND relay_mux_idle_ttl_secs >= relay_mux_probe_interval_secs
            + ((relay_mux_probe_timeout_ms + 999) / 1000)
        -- Business stream IDs are uint16, start at 1, and are not reused.
        AND relay_mux_max_requests_per_worker BETWEEN 1 AND 65535
    );
