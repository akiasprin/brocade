-- One latest diagnostic and schedule per machine/chain, independent of model revisions.
-- Credentials are rebuilt from Serving at claim time and are never persisted in this table.
CREATE TABLE unlock_probe_jobs (
    scope_key TEXT PRIMARY KEY,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE ON UPDATE CASCADE,
    app_id TEXT REFERENCES apps(id) ON DELETE CASCADE ON UPDATE CASCADE,
    chain_id TEXT REFERENCES chains(id) ON DELETE CASCADE ON UPDATE CASCADE,
    configuration_revision BIGINT REFERENCES revisions(id),
    ingress_id TEXT,
    protocol TEXT,
    job_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'complete', 'expired', 'obsolete')),
    trigger TEXT NOT NULL CHECK (trigger IN ('manual', 'scheduled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    claimed_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    next_run_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '24 hours',
    results JSONB NOT NULL DEFAULT '[]'::jsonb,
    path_error TEXT,
    previous_results JSONB NOT NULL DEFAULT '[]'::jsonb,
    previous_completed_at TIMESTAMPTZ,
    FOREIGN KEY (app_id,chain_id) REFERENCES chains(app_id,id) ON UPDATE CASCADE ON DELETE CASCADE,
    CHECK ((app_id IS NULL AND chain_id IS NULL AND configuration_revision IS NULL)
        OR (app_id IS NOT NULL AND chain_id IS NOT NULL AND configuration_revision IS NOT NULL))
);
CREATE INDEX unlock_probe_jobs_worker ON unlock_probe_jobs(node_id, state, created_at);
CREATE TABLE unlock_probe_discovery (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE ON UPDATE CASCADE,
    next_discovery_at TIMESTAMPTZ NOT NULL
);

-- Consolidated independent IPv4 / IPv6 observations.
-- Legacy rows remain automatic; independent jobs use distinct scope keys.
ALTER TABLE unlock_probe_jobs ADD COLUMN address_family TEXT CHECK (address_family IN ('v4','v6'));
CREATE INDEX unlock_probe_jobs_family_worker ON unlock_probe_jobs(node_id,address_family,state,created_at);

-- Consolidated live stale-result refresh policy.
-- This remains independent of model revisions and node configuration.
ALTER TABLE control_state ADD COLUMN unlock_refresh_stale BOOLEAN NOT NULL DEFAULT TRUE;
UPDATE unlock_probe_discovery SET next_discovery_at = LEAST(next_discovery_at, now());

-- Consolidated Agent channel notifications.
-- Notifications are hints delivered only at commit. Existing desired state, jobs and release
-- receipts remain authoritative; reconnect always rechecks them. No credentials enter NOTIFY.
CREATE FUNCTION notify_agent_channel_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('brocade_agent_change', json_build_object(
        'domain', TG_ARGV[0], 'node', CASE WHEN TG_NARGS > 1 THEN COALESCE(to_jsonb(NEW), to_jsonb(OLD))->>TG_ARGV[1] END)::text);
    RETURN NULL;
END;
$$;

CREATE TRIGGER agent_channel_release_created AFTER INSERT ON binary_release_targets
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('release', 'node_id');
CREATE TRIGGER agent_channel_release_progress AFTER UPDATE OF status, attempt ON binary_release_targets
    FOR EACH ROW WHEN ((OLD.status IS DISTINCT FROM NEW.status AND NEW.status = 'pending')
        OR OLD.attempt IS DISTINCT FROM NEW.attempt)
    EXECUTE FUNCTION notify_agent_channel_change('release', 'node_id');
CREATE TRIGGER agent_channel_release_control AFTER UPDATE OF status ON binary_releases
    FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status)
    EXECUTE FUNCTION notify_agent_channel_change('release');
CREATE TRIGGER agent_channel_deployment_created AFTER INSERT ON deployment_targets
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('desired', 'node_id');
CREATE TRIGGER agent_channel_deployment_control AFTER UPDATE OF status ON deployments
    FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status)
    EXECUTE FUNCTION notify_agent_channel_change('desired');
CREATE TRIGGER agent_channel_deployment_wave AFTER INSERT ON deployment_wave_progress
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('desired');
CREATE TRIGGER agent_channel_source_access AFTER UPDATE OF revision ON source_access_state
    FOR EACH ROW WHEN (OLD.revision IS DISTINCT FROM NEW.revision)
    EXECUTE FUNCTION notify_agent_channel_change('source_access');
CREATE TRIGGER agent_channel_traffic_limit AFTER INSERT OR UPDATE OF revision ON node_traffic_limit
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('traffic_limit', 'node_id');
CREATE TRIGGER agent_channel_access_quota AFTER INSERT OR DELETE OR UPDATE OF revision ON user_app_quotas
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('user_traffic');
CREATE TRIGGER agent_channel_forwarding AFTER INSERT OR UPDATE OR DELETE ON forwarding_rules
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('desired');
CREATE TRIGGER agent_channel_forwarding_entitlement AFTER INSERT OR UPDATE OR DELETE ON forwarding_entitlements
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('desired');
CREATE TRIGGER agent_channel_unlock_created AFTER INSERT ON unlock_probe_jobs
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('unlock_probe', 'node_id');
CREATE TRIGGER agent_channel_unlock_queued AFTER UPDATE OF state ON unlock_probe_jobs
    FOR EACH ROW WHEN (NEW.state = 'queued' AND OLD.state IS DISTINCT FROM NEW.state)
    EXECUTE FUNCTION notify_agent_channel_change('unlock_probe', 'node_id');

-- Consolidated path-based unlock readiness.
-- A temporary inability to start a forced-family probe is not a service result.
ALTER TABLE unlock_probe_jobs ADD COLUMN readiness_failures SMALLINT NOT NULL DEFAULT 0
    CHECK (readiness_failures BETWEEN 0 AND 6);

-- Revisit known prerelease failures once with path-based detection. Keep their evidence.
UPDATE unlock_probe_jobs SET next_run_at = LEAST(next_run_at, now())
    WHERE state = 'complete' AND path_error = 'family_unavailable';
UPDATE unlock_probe_discovery SET next_discovery_at = LEAST(next_discovery_at, now())
    WHERE node_id IN (SELECT node_id FROM unlock_probe_jobs
        WHERE state = 'complete' AND path_error = 'family_unavailable');

-- Full diagnostic cycles are independent of path-change refreshes.
ALTER TABLE unlock_probe_jobs ADD COLUMN dns_fingerprint TEXT;
ALTER TABLE control_state
    ADD COLUMN unlock_detection_mode TEXT NOT NULL DEFAULT 'scheduled'
        CHECK (unlock_detection_mode IN ('manual','scheduled')),
    ADD COLUMN unlock_detection_interval_hours INTEGER NOT NULL DEFAULT 24
        CHECK (unlock_detection_interval_hours BETWEEN 1 AND 720),
    ADD COLUMN unlock_detection_requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN unlock_detection_next_run_at TIMESTAMPTZ DEFAULT now() + interval '24 hours';

-- Consolidated online source session history.
-- Each completed ingress/IP epoch survives reconnects. Only the live epoch is upserted.
ALTER TABLE user_online_sources
    ADD COLUMN session_id BIGINT GENERATED ALWAYS AS IDENTITY,
    DROP CONSTRAINT user_online_sources_pkey,
    ADD PRIMARY KEY (session_id);
CREATE UNIQUE INDEX user_online_sources_active_epoch
    ON user_online_sources (node_id, tenant_id, user_id, ingress_id, source_ip) WHERE active;
CREATE INDEX user_online_sources_user_history
    ON user_online_sources (tenant_id, user_id, last_observed_at DESC, session_id DESC)
    WHERE NOT active;
-- Retention also removes stale active snapshots left behind by nodes that stopped reporting.
DROP INDEX user_online_sources_retention_idx;
CREATE INDEX user_online_sources_retention_idx ON user_online_sources (last_observed_at);

-- BEGIN USER CYCLE RESET
-- A reset changes the quota view, never the immutable traffic ledger or counter heads.
CREATE TABLE user_cycle_resets (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    request_id TEXT NOT NULL CHECK (request_id ~ '^[0-9a-f]{32}$'),
    previous_reset_id BIGINT,
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL CHECK (period_end>period_start),
    reset_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    baseline_starts_at TIMESTAMPTZ NOT NULL,
    actor TEXT NOT NULL,
    UNIQUE (tenant_id,user_id,request_id),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE TABLE user_cycle_reset_state (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    reset_id BIGINT NOT NULL REFERENCES user_cycle_resets(id),
    PRIMARY KEY (tenant_id,user_id),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE TABLE user_cycle_access_baselines (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    app_id TEXT NOT NULL,
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    uplink_bytes BIGINT NOT NULL CHECK (uplink_bytes>=0),
    downlink_bytes BIGINT NOT NULL CHECK (downlink_bytes>=0),
    PRIMARY KEY (tenant_id,user_id,app_id,period_start,period_end),
    FOREIGN KEY (tenant_id,user_id) REFERENCES user_cycle_reset_state ON DELETE CASCADE
);
CREATE TABLE user_cycle_forwarding_baselines (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    rule_id BIGINT NOT NULL,
    period_start TIMESTAMPTZ NOT NULL,
    uplink_bytes BIGINT NOT NULL CHECK (uplink_bytes>=0),
    downlink_bytes BIGINT NOT NULL CHECK (downlink_bytes>=0),
    PRIMARY KEY (tenant_id,user_id,node_id,rule_id,period_start),
    FOREIGN KEY (tenant_id,user_id) REFERENCES user_cycle_reset_state ON DELETE CASCADE
);

CREATE FUNCTION user_cycle_access_usage(owner_tenant TEXT,owner_user TEXT,starts_at TIMESTAMPTZ,ends_at TIMESTAMPTZ)
RETURNS TABLE(app_id TEXT,uplink_bytes BIGINT,downlink_bytes BIGINT,has_gap BOOLEAN)
LANGUAGE SQL STABLE AS $$
    SELECT s.app_id,
           sum(GREATEST(0,s.uplink_bytes-coalesce(b.uplink_bytes,0)))::bigint,
           sum(GREATEST(0,s.downlink_bytes-coalesce(b.downlink_bytes,0)))::bigint,
           bool_or(s.has_gap)
    FROM usage_rollups s
    LEFT JOIN user_cycle_reset_state state USING(tenant_id,user_id)
    LEFT JOIN user_cycle_resets reset ON reset.id=state.reset_id
    LEFT JOIN user_cycle_access_baselines b ON b.tenant_id=s.tenant_id AND b.user_id=s.user_id
      AND b.app_id=s.app_id AND b.period_start=s.period_start AND b.period_end=s.period_end
      AND reset.reset_at>=starts_at AND reset.reset_at<ends_at
    WHERE s.tenant_id=owner_tenant AND s.user_id=owner_user
      AND s.period_start>=starts_at AND s.period_start<ends_at
    GROUP BY s.app_id
$$;
CREATE FUNCTION user_cycle_forwarding_usage(owner_tenant TEXT,owner_user TEXT,starts_at TIMESTAMPTZ,ends_at TIMESTAMPTZ)
RETURNS TABLE(node_id TEXT,rule_id BIGINT,uplink_bytes BIGINT,downlink_bytes BIGINT,has_gap BOOLEAN)
LANGUAGE SQL STABLE AS $$
    SELECT s.node_id,s.rule_id,
           sum(GREATEST(0,s.uplink_bytes-coalesce(b.uplink_bytes,0)))::bigint,
           sum(GREATEST(0,s.downlink_bytes-coalesce(b.downlink_bytes,0)))::bigint,
           bool_or(s.has_gap)
    FROM forwarding_usage_rollups s
    LEFT JOIN user_cycle_reset_state state USING(tenant_id,user_id)
    LEFT JOIN user_cycle_resets reset ON reset.id=state.reset_id
    LEFT JOIN user_cycle_forwarding_baselines b ON b.tenant_id=s.tenant_id AND b.user_id=s.user_id
      AND b.node_id=s.node_id AND b.rule_id=s.rule_id AND b.period_start=s.period_start
      AND reset.reset_at>=starts_at AND reset.reset_at<ends_at
    WHERE s.tenant_id=owner_tenant AND s.user_id=owner_user
      AND s.period_start>=starts_at AND s.period_start<ends_at
    GROUP BY s.node_id,s.rule_id
$$;
CREATE OR REPLACE FUNCTION user_traffic_usage(owner_tenant TEXT,owner_user TEXT,traffic_kind TEXT,app_scope TEXT DEFAULT '')
RETURNS BIGINT LANGUAGE SQL STABLE AS $$
    SELECT (CASE traffic_kind WHEN 'access' THEN
        (SELECT COALESCE(sum(s.uplink_bytes+s.downlink_bytes),0)
         FROM user_cycle_access_usage(u.tenant_id,u.id,c.starts_at,c.ends_at) s WHERE s.app_id=app_scope)
        WHEN 'forwarding' THEN
        (SELECT COALESCE(sum(s.uplink_bytes+s.downlink_bytes),0)
         FROM user_cycle_forwarding_usage(u.tenant_id,u.id,c.starts_at,c.ends_at) s)
        END * CASE u.traffic_accounting_mode WHEN 'double' THEN 2 ELSE 1 END)::bigint
    FROM users u CROSS JOIN LATERAL user_traffic_cycle(u.traffic_reset_day,now()) c
    WHERE u.tenant_id=owner_tenant AND u.id=owner_user
$$;

-- Each accepted sample adjusts its pre-reset portion exactly once, in the same transaction
-- as the raw ledger. Measured intervals crossing the cutoff use integer time apportionment.
-- A legacy forwarding reading without a window is attributed by its reading timestamp.
CREATE FUNCTION user_cycle_excluded_bytes(bytes BIGINT,window_start TIMESTAMPTZ,window_end TIMESTAMPTZ,cutoff TIMESTAMPTZ)
RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS $$
    SELECT CASE WHEN window_end<=cutoff THEN bytes
                WHEN window_start IS NULL OR window_start>=cutoff THEN 0
                ELSE trunc(bytes::numeric*extract(epoch FROM cutoff-window_start)
                           /extract(epoch FROM window_end-window_start))::bigint END
$$;
CREATE FUNCTION user_cycle_account_late_sample() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    reset RECORD;
    sample_start TIMESTAMPTZ;
    sample_end TIMESTAMPTZ;
    day_start TIMESTAMPTZ;
    excluded_up BIGINT;
    excluded_down BIGINT;
BEGIN
    -- The reset takes FOR UPDATE on this same parent before reading ledger baselines.
    -- A report waiting here reads the committed new marker in the following statement.
    PERFORM 1 FROM users WHERE tenant_id=NEW.tenant_id AND id=NEW.user_id FOR SHARE;
    SELECT event.* INTO reset FROM user_cycle_reset_state state
      JOIN user_cycle_resets event ON event.id=state.reset_id
      WHERE state.tenant_id=NEW.tenant_id AND state.user_id=NEW.user_id;
    IF NOT FOUND THEN RETURN NEW; END IF;
    sample_start=NEW.window_start;
    IF TG_TABLE_NAME='usage_samples' THEN sample_end=NEW.window_end;
    ELSE sample_end=NEW.read_at; END IF;
    day_start=date_trunc('day',coalesce(sample_start,sample_end) AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong';
    IF day_start<reset.baseline_starts_at THEN RETURN NEW; END IF;
    excluded_up=user_cycle_excluded_bytes(NEW.uplink_bytes,sample_start,sample_end,reset.reset_at);
    excluded_down=user_cycle_excluded_bytes(NEW.downlink_bytes,sample_start,sample_end,reset.reset_at);
    IF excluded_up=0 AND excluded_down=0 THEN RETURN NEW; END IF;
    IF TG_TABLE_NAME='usage_samples' THEN
        IF NEW.app_id IS NULL THEN RETURN NEW; END IF;
        INSERT INTO user_cycle_access_baselines(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes)
        VALUES(NEW.tenant_id,NEW.user_id,NEW.app_id,day_start,day_start+interval '24 hours',excluded_up,excluded_down)
        ON CONFLICT(tenant_id,user_id,app_id,period_start,period_end) DO UPDATE SET
          uplink_bytes=user_cycle_access_baselines.uplink_bytes+EXCLUDED.uplink_bytes,
          downlink_bytes=user_cycle_access_baselines.downlink_bytes+EXCLUDED.downlink_bytes;
    ELSE
        INSERT INTO user_cycle_forwarding_baselines(tenant_id,user_id,node_id,rule_id,period_start,uplink_bytes,downlink_bytes)
        VALUES(NEW.tenant_id,NEW.user_id,NEW.node_id,NEW.rule_id,day_start,excluded_up,excluded_down)
        ON CONFLICT(tenant_id,user_id,node_id,rule_id,period_start) DO UPDATE SET
          uplink_bytes=user_cycle_forwarding_baselines.uplink_bytes+EXCLUDED.uplink_bytes,
          downlink_bytes=user_cycle_forwarding_baselines.downlink_bytes+EXCLUDED.downlink_bytes;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER user_cycle_access_late_sample AFTER INSERT ON usage_samples
    FOR EACH ROW EXECUTE FUNCTION user_cycle_account_late_sample();
CREATE TRIGGER user_cycle_forwarding_late_sample AFTER INSERT ON forwarding_usage_samples
    FOR EACH ROW EXECUTE FUNCTION user_cycle_account_late_sample();
-- END USER CYCLE RESET
