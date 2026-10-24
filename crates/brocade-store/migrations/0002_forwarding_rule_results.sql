-- Legacy receipts stay NULL until a later Agent round supplies per-rule evidence.
-- SQLx applies this migration and its history record in the same transaction.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';

ALTER TABLE forwarding_node_state
    ADD COLUMN rule_results JSONB,
    ADD CONSTRAINT forwarding_node_state_rule_results CHECK (
        rule_results IS NULL OR jsonb_typeof(rule_results) = 'object'
    );

-- Per-machine operational traffic protection; independent of model revisions and releases.
CREATE TABLE node_traffic_limit (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    revision BIGINT DEFAULT 0 NOT NULL CHECK (revision >= 0),
    policy JSONB DEFAULT '{"enabled":false,"rules":[]}' NOT NULL,
    observation JSONB,
    reported_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ DEFAULT now() NOT NULL,
    updated_by TEXT,
    CONSTRAINT node_traffic_limit_policy_shape CHECK (
        jsonb_typeof(policy) = 'object'
        AND policy ? 'enabled' AND policy ? 'rules'
        AND COALESCE(policy ->> 'layer', 'application') IN ('system', 'application', 'both')
        AND jsonb_typeof(policy -> 'enabled') = 'boolean'
        AND jsonb_typeof(policy -> 'rules') = 'array'
        AND jsonb_array_length(policy -> 'rules') <= 8
    ),
    CONSTRAINT node_traffic_limit_observation_shape CHECK (
        observation IS NULL OR jsonb_typeof(observation) = 'object'
    )
);

CREATE TABLE node_traffic_limit_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    policy JSONB NOT NULL,
    actor TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT now() NOT NULL
);
CREATE INDEX node_traffic_limit_events_node_idx ON node_traffic_limit_events (node_id, id DESC);

-- User billing/service policy is operational state and survives model rollback. Only the
-- persisted suspension changes the compiled grant list; compilation never reads the clock.
ALTER TABLE users
    ADD COLUMN traffic_reset_day INTEGER NOT NULL DEFAULT 1 CHECK (traffic_reset_day BETWEEN 1 AND 31),
    ADD COLUMN service_starts_at TIMESTAMPTZ,
    ADD COLUMN service_expires_at TIMESTAMPTZ,
    ADD COLUMN service_suspended BOOLEAN NOT NULL DEFAULT FALSE,
    ADD CONSTRAINT users_service_period CHECK (
        service_starts_at IS NULL OR service_expires_at IS NULL OR service_expires_at > service_starts_at
    );

CREATE TABLE user_service_policy_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    policy JSONB NOT NULL,
    actor TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, user_id) REFERENCES users(tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX user_service_policy_events_owner ON user_service_policy_events (tenant_id, user_id, id DESC);

CREATE FUNCTION user_service_state(starts_at TIMESTAMPTZ, expires_at TIMESTAMPTZ, at_time TIMESTAMPTZ)
RETURNS TEXT LANGUAGE SQL IMMUTABLE AS $$
    SELECT CASE WHEN starts_at > at_time THEN 'pending'
                WHEN expires_at <= at_time THEN 'expired' ELSE 'active' END
$$;

-- Each boundary is anchored to its own calendar month: February's clamp must not move
-- a day-31 policy to day 28 in March. All callers use these same UTC+8 day boundaries.
CREATE FUNCTION user_traffic_cycle(reset_day INTEGER, at_time TIMESTAMPTZ)
RETURNS TABLE(starts_at TIMESTAMPTZ, ends_at TIMESTAMPTZ)
LANGUAGE SQL STABLE STRICT AS $$
    WITH months AS (
        SELECT date_trunc('month', at_time AT TIME ZONE 'Asia/Hong_Kong')
               + offset_month * interval '1 month' AS month
        FROM generate_series(-1, 1) AS offset_month
    ), boundaries AS (
        SELECT (month + (LEAST(reset_day, extract(day FROM month + interval '1 month - 1 day')::integer) - 1)
                 * interval '1 day') AT TIME ZONE 'Asia/Hong_Kong' AS boundary
        FROM months
    )
    SELECT max(boundary) FILTER (WHERE boundary <= at_time),
           min(boundary) FILTER (WHERE boundary > at_time) FROM boundaries
$$;

-- Preserve serving evidence before allowing runtime probe history to expire.
-- New structures and their backfill commit atomically before the new cleaner can run.
SET LOCAL statement_timeout = '120s';
LOCK TABLE vpngate_probe_samples IN SHARE ROW EXCLUSIVE MODE;

CREATE TABLE vpngate_runtime_evidence (
    node_id TEXT NOT NULL,
    outbound_id TEXT NOT NULL,
    server_id TEXT NOT NULL REFERENCES vpngate_servers(id),
    profile_sha256 TEXT NOT NULL REFERENCES vpngate_profiles(sha256),
    evidence_kind TEXT NOT NULL CHECK (evidence_kind IN ('latest', 'success', 'connect')),
    sample_id BIGINT NOT NULL,
    topology_revision BIGINT NOT NULL,
    catalog_generation BIGINT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('succeeded', 'failed')),
    exit_ip INET,
    exit_country_code TEXT,
    connect_ms INTEGER,
    download_bps BIGINT,
    ip_scores JSONB NOT NULL,
    ip_networks JSONB NOT NULL,
    error_code TEXT,
    error_detail TEXT,
    probed_at TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (node_id, outbound_id, server_id, profile_sha256, evidence_kind)
) WITH (fillfactor = 85, autovacuum_vacuum_scale_factor = 0.02, autovacuum_analyze_scale_factor = 0.02);

INSERT INTO vpngate_runtime_evidence (sample_id, node_id, outbound_id, topology_revision, catalog_generation, server_id, profile_sha256, status, exit_ip, exit_country_code, connect_ms, download_bps, ip_scores, ip_networks, error_code, error_detail, probed_at, received_at, evidence_kind)
SELECT DISTINCT ON (sample.node_id, sample.outbound_id, sample.server_id, sample.profile_sha256, kind.name)
       sample.id, sample.node_id, sample.outbound_id, sample.topology_revision, sample.catalog_generation, sample.server_id, sample.profile_sha256, sample.status, sample.exit_ip, sample.exit_country_code, sample.connect_ms, sample.download_bps, sample.ip_scores, sample.ip_networks, sample.error_code, sample.error_detail, sample.probed_at, sample.received_at, kind.name
  FROM vpngate_probe_samples sample
 CROSS JOIN (VALUES ('latest'), ('success'), ('connect')) kind(name)
 WHERE kind.name = 'latest'
    OR (sample.status = 'succeeded' AND (kind.name = 'success' OR sample.connect_ms > 0))
 ORDER BY sample.node_id, sample.outbound_id, sample.server_id, sample.profile_sha256,
          kind.name, sample.probed_at DESC, sample.id DESC;

CREATE INDEX vpngate_runtime_evidence_pool_recent
    ON vpngate_runtime_evidence (node_id, outbound_id, server_id, evidence_kind, probed_at DESC, sample_id DESC);
CREATE INDEX vpngate_runtime_evidence_success
    ON vpngate_runtime_evidence (server_id, profile_sha256, probed_at DESC, node_id, exit_ip)
    WHERE evidence_kind = 'success';
-- Serving queries now use the projection; retain only history identity/dedup and expiry indexes.
DROP INDEX vpngate_probe_samples_pool_recent;
DROP INDEX vpngate_probe_samples_server_recent;
DROP INDEX vpngate_probe_samples_success_evidence;
CREATE INDEX vpngate_probe_samples_received_brin
    ON vpngate_probe_samples USING brin (received_at);

-- One replaceable status per allow-listed table, never an unbounded execution log.
CREATE TABLE data_retention_cleanup (
    table_name TEXT PRIMARY KEY,
    ttl_hours INTEGER NOT NULL CHECK (ttl_hours BETWEEN 1 AND 87600),
    finished_at TIMESTAMPTZ NOT NULL,
    last_success_at TIMESTAMPTZ,
    deleted_rows BIGINT NOT NULL DEFAULT 0 CHECK (deleted_rows >= 0),
    error_code TEXT,
    has_backlog BOOLEAN,
    next_run_at TIMESTAMPTZ NOT NULL
);

-- Access quotas remain owned by (tenant,user,app); add only exhaustion handling.
ALTER TABLE user_app_quotas
    ADD COLUMN exhausted_action TEXT NOT NULL DEFAULT 'suspend' CHECK (exhausted_action IN ('suspend','throttle')),
    ADD COLUMN rate_bps BIGINT,
    ADD COLUMN exhausted BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    ADD CONSTRAINT user_app_quota_action_rate CHECK (
        (exhausted_action='suspend' AND rate_bps IS NULL) OR
        (exhausted_action='throttle' AND rate_bps BETWEEN 1000 AND 1000000000000 AND rate_bps IS NOT NULL));

-- Forwarding has an independent user quota and never consumes an app's access quota.
CREATE TABLE user_traffic_policies (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind = 'forwarding'),
    quota_bytes BIGINT CHECK (quota_bytes > 0),
    action TEXT NOT NULL CHECK (action IN ('suspend','throttle')),
    rate_bps BIGINT,
    exhausted BOOLEAN NOT NULL DEFAULT FALSE,
    suspended BOOLEAN NOT NULL DEFAULT FALSE,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by TEXT NOT NULL,
    PRIMARY KEY (tenant_id,user_id,kind),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE,
    CHECK ((action='suspend' AND rate_bps IS NULL) OR
           (action='throttle' AND rate_bps IS NOT NULL AND rate_bps BETWEEN 1000 AND 1000000000000)),
    CHECK (NOT suspended OR (exhausted AND action='suspend'))
);
CREATE VIEW user_traffic_scopes AS
    SELECT tenant_id,user_id,'access'::text AS kind,app_id AS scope,
           limit_bytes AS quota_bytes,exhausted_action AS action,rate_bps,exhausted,
           exhausted AND exhausted_action='suspend' AS suspended,revision
    FROM user_app_quotas
    UNION ALL
    SELECT tenant_id,user_id,kind,''::text AS scope,quota_bytes,action,rate_bps,exhausted,suspended,revision
    FROM user_traffic_policies;
CREATE TABLE user_traffic_policy_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('access','forwarding')),
    scope TEXT NOT NULL,
    policy JSONB NOT NULL,
    actor TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE,
    CHECK ((kind='access' AND scope<>'') OR (kind='forwarding' AND scope=''))
);
CREATE INDEX user_traffic_policy_events_owner ON user_traffic_policy_events(tenant_id,user_id,kind,scope,id DESC);

-- One durable pacing clock per user/app or user/forwarding, shared across machines.
CREATE TABLE user_traffic_budgets (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('access','forwarding')),
    scope TEXT NOT NULL,
    next_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,user_id,kind,scope),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE,
    CHECK ((kind='access' AND scope<>'') OR (kind='forwarding' AND scope=''))
);
CREATE TABLE user_traffic_budget_receipts (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    scope TEXT NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    bytes BIGINT NOT NULL CHECK (bytes > 0),
    rate_bps BIGINT NOT NULL CHECK (rate_bps > 0),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tenant_id,user_id,kind,scope,node_id,request_id),
    FOREIGN KEY (tenant_id,user_id,kind,scope) REFERENCES user_traffic_budgets ON DELETE CASCADE
);
CREATE INDEX user_traffic_receipts_expiry ON user_traffic_budget_receipts(tenant_id,user_id,kind,scope,expires_at);
CREATE TABLE user_traffic_budget_demand (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    scope TEXT NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    served_at TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
    PRIMARY KEY(tenant_id,user_id,kind,scope,node_id),
    FOREIGN KEY(tenant_id,user_id,kind,scope) REFERENCES user_traffic_budgets ON DELETE CASCADE
);
CREATE FUNCTION user_traffic_quota_deleted() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME='user_app_quotas' THEN
        DELETE FROM user_traffic_budgets WHERE tenant_id=OLD.tenant_id AND user_id=OLD.user_id AND kind='access' AND scope=OLD.app_id;
    ELSE
        DELETE FROM user_traffic_budgets WHERE tenant_id=OLD.tenant_id AND user_id=OLD.user_id AND kind='forwarding' AND scope='';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER user_app_traffic_quota_deleted AFTER DELETE ON user_app_quotas
    FOR EACH ROW EXECUTE FUNCTION user_traffic_quota_deleted();
CREATE TRIGGER user_forwarding_traffic_quota_deleted AFTER DELETE ON user_traffic_policies
    FOR EACH ROW EXECUTE FUNCTION user_traffic_quota_deleted();
ALTER TABLE nodes ADD COLUMN user_traffic_supported BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN user_traffic_support_at TIMESTAMPTZ;
CREATE FUNCTION user_global_suspended(owner_tenant TEXT, owner_user TEXT, traffic_kind TEXT)
RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
    SELECT COALESCE((SELECT suspended FROM user_traffic_policies
        WHERE tenant_id=owner_tenant AND user_id=owner_user AND kind=traffic_kind),FALSE)
$$;
CREATE FUNCTION user_global_configured(owner_tenant TEXT, owner_user TEXT, traffic_kind TEXT)
RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
    SELECT EXISTS(SELECT 1 FROM user_traffic_policies
        WHERE tenant_id=owner_tenant AND user_id=owner_user AND kind=traffic_kind)
$$;
CREATE FUNCTION user_traffic_usage(owner_tenant TEXT, owner_user TEXT, traffic_kind TEXT, app_scope TEXT DEFAULT '')
RETURNS BIGINT LANGUAGE SQL STABLE AS $$
    SELECT (CASE traffic_kind WHEN 'access' THEN
        (SELECT COALESCE(sum(s.uplink_bytes+s.downlink_bytes),0) FROM usage_rollups s
         WHERE s.tenant_id=u.tenant_id AND s.user_id=u.id AND s.app_id=app_scope AND s.period_start>=c.starts_at AND s.period_start<c.ends_at)
        WHEN 'forwarding' THEN
        (SELECT COALESCE(sum(s.uplink_bytes+s.downlink_bytes),0) FROM forwarding_usage_rollups s
         WHERE s.tenant_id=u.tenant_id AND s.user_id=u.id AND s.period_start>=c.starts_at AND s.period_start<c.ends_at)
        END * CASE u.traffic_accounting_mode WHEN 'double' THEN 2 ELSE 1 END)::bigint
    FROM users u CROSS JOIN LATERAL user_traffic_cycle(u.traffic_reset_day,now()) c
    WHERE u.tenant_id=owner_tenant AND u.id=owner_user
$$;

-- Source admission is operational state: model rollback must not release a banned peer.
CREATE TABLE source_access_state (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0)
);
INSERT INTO source_access_state DEFAULT VALUES;
CREATE TABLE source_access_policies (
    scope_key TEXT PRIMARY KEY,
    tenant_id TEXT,
    user_id TEXT,
    node_id TEXT REFERENCES nodes(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision > 0),
    ips JSONB NOT NULL CHECK (jsonb_typeof(ips) = 'array' AND jsonb_array_length(ips) <= 256),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by TEXT NOT NULL,
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE,
    CHECK ((node_id IS NOT NULL AND tenant_id IS NULL AND user_id IS NULL AND scope_key='node:'||node_id)
        OR (node_id IS NULL AND tenant_id IS NOT NULL AND user_id IS NOT NULL AND scope_key='user:'||tenant_id||'/'||user_id))
);
CREATE TABLE source_access_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    scope_key TEXT NOT NULL,
    revision BIGINT NOT NULL,
    ips JSONB NOT NULL,
    actor TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE source_access_node_state (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision >= 0),
    supported BOOLEAN NOT NULL,
    error TEXT CHECK (length(error) <= 512),
    reported_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Cascading owner removal must invalidate conditional Agent snapshots as well.
CREATE FUNCTION source_access_policy_deleted() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE next_revision BIGINT;
BEGIN
    UPDATE source_access_state SET revision=revision+1 WHERE singleton RETURNING revision INTO next_revision;
    INSERT INTO source_access_events(scope_key,revision,ips,actor)
        VALUES(OLD.scope_key,next_revision,'[]'::jsonb,'system:source-access-owner-deleted');
    RETURN OLD;
END
$$;
CREATE TRIGGER source_access_policy_deleted AFTER DELETE ON source_access_policies
    FOR EACH ROW EXECUTE FUNCTION source_access_policy_deleted();
ALTER TABLE nodes
    ADD COLUMN user_traffic_applied_hash TEXT CHECK (user_traffic_applied_hash IS NULL OR user_traffic_applied_hash ~ '^[0-9a-f]{64}$'),
    ADD COLUMN user_traffic_policy_seen_at TIMESTAMPTZ;

-- Consolidated tunnel target strategy.
ALTER TABLE external_outbounds
    ADD COLUMN target_strategy TEXT,
    ADD CONSTRAINT external_outbounds_target_strategy_check CHECK (
        target_strategy IN ('as_is', 'use_ip', 'use_ipv4', 'use_ipv6', 'use_ipv4v6', 'use_ipv6v4')
    );

-- Consolidated forwarding total-quota retirement.
-- Retire user-wide forwarding caps. Preserve their last configuration as inert audit data,
-- while leaving the access branch and machine-local forwarding quota records intact.
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='120s';

CREATE OR REPLACE VIEW user_traffic_scopes AS
    SELECT tenant_id,user_id,'access'::text AS kind,app_id AS scope,
           limit_bytes AS quota_bytes,exhausted_action AS action,rate_bps,exhausted,
           exhausted AND exhausted_action='suspend' AS suspended,revision
    FROM user_app_quotas;

DROP FUNCTION user_global_suspended(TEXT, TEXT, TEXT);
DROP FUNCTION user_global_configured(TEXT, TEXT, TEXT);
DROP TRIGGER user_forwarding_traffic_quota_deleted ON user_traffic_policies;
ALTER TABLE user_traffic_policies RENAME TO retired_forwarding_traffic_policies;
COMMENT ON TABLE retired_forwarding_traffic_policies IS
    'Archived forwarding total-quota configuration; never consulted by policy enforcement.';

-- Access quota deletion keeps the same budget cleanup, with no retired forwarding trigger.
CREATE OR REPLACE FUNCTION user_traffic_quota_deleted() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM user_traffic_budgets
    WHERE tenant_id=OLD.tenant_id AND user_id=OLD.user_id AND kind='access' AND scope=OLD.app_id;
    RETURN OLD;
END
$$;
DELETE FROM user_traffic_budgets WHERE kind='forwarding';
ALTER TABLE user_traffic_budgets
    DROP CONSTRAINT user_traffic_budgets_kind_check,
    ADD CONSTRAINT user_traffic_budgets_kind_check CHECK (kind='access');

-- Force existing Agents to reconcile rules that were hidden by a total-quota suspension.
INSERT INTO forwarding_node_state (node_id, desired_generation, reconcile_status)
SELECT DISTINCT e.node_id,1,'pending'
FROM forwarding_entitlements e
JOIN retired_forwarding_traffic_policies p USING(tenant_id,user_id)
ON CONFLICT (node_id) DO UPDATE SET
    desired_generation=forwarding_node_state.desired_generation+1,
    reconcile_status='pending',last_error=NULL;

-- A total cap previously cleared machine suspension flags. Restore those flags, and their
-- auditable jobs, before the new Console can serve desired state. Limits and usage stay intact.
DO $$
DECLARE
    quota RECORD;
    next_job BIGINT;
BEGIN
    FOR quota IN
        SELECT q.tenant_id,q.user_id,q.node_id,q.limit_bytes,q.suspended,
               state.desired_generation AS generation,
               COALESCE((SELECT sum(s.uplink_bytes+s.downlink_bytes)::bigint
                   FROM forwarding_usage_rollups s
                   WHERE s.tenant_id=q.tenant_id AND s.user_id=q.user_id AND s.node_id=q.node_id
                     AND s.period_start>=cycle.starts_at AND s.period_start<cycle.ends_at),0)
                   * CASE u.traffic_accounting_mode WHEN 'double' THEN 2 ELSE 1 END AS used_bytes
        FROM forwarding_node_quotas q
        JOIN retired_forwarding_traffic_policies p USING(tenant_id,user_id)
        JOIN users u ON u.tenant_id=q.tenant_id AND u.id=q.user_id
        CROSS JOIN LATERAL user_traffic_cycle(u.traffic_reset_day,now()) cycle
        JOIN forwarding_node_state state ON state.node_id=q.node_id
        ORDER BY q.node_id COLLATE "C",q.tenant_id,q.user_id
        FOR UPDATE OF q
    LOOP
        -- Record the retirement at the current generation even when the machine flag did not
        -- change. Supersede earlier pending intent together with this new audit job.
        UPDATE jobs SET status='canceled',last_error='机器转发额度恢复独立执行',updated_at=now()
        WHERE kind='forwarding-quota' AND status IN ('queued','running')
          AND payload->>'tenant_id'=quota.tenant_id AND payload->>'user_id'=quota.user_id
          AND payload->>'node_id'=quota.node_id;
        INSERT INTO jobs(kind,status,payload)
        VALUES('forwarding-quota','queued',jsonb_build_object(
            'tenant_id',quota.tenant_id,'user_id',quota.user_id,'node_id',quota.node_id,
            'generation',quota.generation,
            'suspended',quota.limit_bytes IS NOT NULL AND quota.used_bytes>=quota.limit_bytes,
            'limit_bytes',quota.limit_bytes,'used_bytes',quota.used_bytes,
            'requested_by','system:remove-forwarding-total-quota',
            'note','移除转发总额度，恢复机器独立额度'))
        RETURNING id INTO next_job;
        UPDATE forwarding_node_quotas SET
            suspended=quota.limit_bytes IS NOT NULL AND quota.used_bytes>=quota.limit_bytes,
            last_job_id=next_job
        WHERE tenant_id=quota.tenant_id AND user_id=quota.user_id AND node_id=quota.node_id;
    END LOOP;
END
$$;
