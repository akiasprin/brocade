-- User Clash routing preferences are operational state, never revisioned model data.
CREATE TABLE user_clash_rules (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    rules JSONB NOT NULL CHECK (jsonb_typeof(rules) = 'object'),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,user_id),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);

-- Optional host-egress maintenance; independent of proxy model revisions.

-- Machine operational intent never changes a proxy model revision.
CREATE TABLE ip_maintenance_policies (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    generation BIGINT NOT NULL DEFAULT 0 CHECK (generation >= 0),
    policy JSONB NOT NULL CHECK (jsonb_typeof(policy)='object'),
    control_sequence BIGINT NOT NULL DEFAULT 0 CHECK (control_sequence >= 0),
    manual_request_id TEXT,
    stop_sequence BIGINT NOT NULL DEFAULT 0 CHECK (stop_sequence >= 0),
    target_round_id TEXT,
    manual_sequence BIGINT NOT NULL DEFAULT 0 CHECK (manual_sequence >= 0),
    manual_expires_at BIGINT,
    session_generation BIGINT NOT NULL DEFAULT 0 CHECK (session_generation >= 0),
    actor_id TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE ip_maintenance_state (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    report JSONB NOT NULL CHECK (jsonb_typeof(report)='object'),
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE ip_maintenance_runs (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    id BIGSERIAL UNIQUE NOT NULL,
    round_id TEXT NOT NULL CHECK (length(round_id) BETWEEN 1 AND 80),
    report JSONB NOT NULL CHECK (jsonb_typeof(report)='object'),
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (node_id,round_id)
);
CREATE INDEX ip_maintenance_runs_recent ON ip_maintenance_runs(node_id,id DESC);
CREATE INDEX ip_maintenance_runs_expiry ON ip_maintenance_runs(received_at);
CREATE TABLE ip_maintenance_events (
    id BIGSERIAL PRIMARY KEY,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    generation BIGINT NOT NULL,
    actor_id TEXT NOT NULL,
    event JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ip_maintenance_events_recent ON ip_maintenance_events(node_id,id DESC);
CREATE TRIGGER agent_channel_ip_maintenance AFTER INSERT OR UPDATE ON ip_maintenance_policies
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('desired','node_id');
CREATE TABLE ip_maintenance_assets (
    key TEXT PRIMARY KEY CHECK (length(key) BETWEEN 1 AND 180),
    payload JSONB NOT NULL,
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[a-f0-9]{64}$'),
    source_commit TEXT NOT NULL CHECK (source_commit ~ '^[a-f0-9]{40}$'),
    fetched_at BIGINT NOT NULL CHECK (fetched_at>0)
);
CREATE TABLE ip_maintenance_refresh (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    generation BIGINT NOT NULL DEFAULT 0 CHECK (generation>=0),
    lease_until TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
    next_run_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error TEXT CHECK (length(last_error)<=200)
);
INSERT INTO ip_maintenance_refresh(id) VALUES(TRUE);

CREATE TABLE ip_maintenance_commands (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 80),
    kind TEXT NOT NULL CHECK (kind IN ('run','stop','session_reset')),
    expected_generation BIGINT NOT NULL,
    target_round_id TEXT,
    control_sequence BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(node_id,request_id)
);

-- Consolidated per-node public-IP bell preferences.
-- An absent row inherits the enabled default. This is live presentation policy, never model intent.
CREATE TABLE node_notification_preferences (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    public_ip_changes BOOLEAN NOT NULL DEFAULT TRUE,
    changed_by TEXT NOT NULL CHECK (length(btrim(changed_by)) BETWEEN 1 AND 256),
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
