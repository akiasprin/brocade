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
