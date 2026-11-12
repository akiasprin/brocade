-- Per-user Sub-Store configuration and durable execution history.
CREATE TABLE subscription_connectors (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    sealed TEXT,
    metadata JSONB NOT NULL,
    remote_name TEXT NOT NULL UNIQUE,
    auto BOOLEAN NOT NULL DEFAULT FALSE,
    registered BOOLEAN NOT NULL DEFAULT FALSE,
    verified BOOLEAN NOT NULL DEFAULT FALSE,
    source_version BIGINT,
    source_fingerprint TEXT,
    last_confirmed TIMESTAMPTZ,
    next_check TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,user_id),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE TABLE subscription_connector_runs (
    id BIGSERIAL PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    config_version BIGINT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('sync','verify','inspect','cleanup')),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued','running','retry_wait','succeeded','partial','failed','canceled','superseded')),
    trigger TEXT NOT NULL,
    actor TEXT NOT NULL,
    parent BIGINT,
    source_version BIGINT,
    output_count INTEGER,
    error TEXT,
    attempts JSONB NOT NULL DEFAULT '[]',
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at TIMESTAMPTZ,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_owner TEXT,
    lease_until TIMESTAMPTZ,
    probe_token TEXT,
    observed_revision BIGINT,
    observed_fingerprint TEXT,
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX subscription_connector_one_active ON subscription_connector_runs(tenant_id,user_id) WHERE status IN ('queued','running','retry_wait');
CREATE INDEX subscription_connector_history ON subscription_connector_runs(tenant_id,user_id,id DESC);
CREATE INDEX subscription_connector_worker ON subscription_connector_runs(available_at,id) WHERE status IN ('queued','retry_wait','running');

CREATE INDEX subscription_connector_receipt ON subscription_connector_runs(probe_token) WHERE status='running';

-- Invalidate observations at the same serving boundary as the public subscription, never on drafts.
CREATE FUNCTION invalidate_subscription_connectors() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE subscription_connectors SET verified=false,next_check=now() WHERE sealed IS NOT NULL;
    RETURN NEW;
END $$;
CREATE TRIGGER subscription_connector_serving AFTER UPDATE OF generation ON subscription_serving_state
    FOR EACH ROW WHEN (OLD.generation IS DISTINCT FROM NEW.generation) EXECUTE FUNCTION invalidate_subscription_connectors();
CREATE TRIGGER subscription_connector_origin AFTER UPDATE OF agent_public_url ON control_state
    FOR EACH ROW WHEN (OLD.agent_public_url IS DISTINCT FROM NEW.agent_public_url) EXECUTE FUNCTION invalidate_subscription_connectors();
CREATE FUNCTION wake_user_subscription_connector() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE subscription_connectors SET verified=false,next_check=now() WHERE tenant_id=NEW.tenant_id AND user_id=NEW.id;
    RETURN NEW;
END $$;
CREATE TRIGGER subscription_connector_user AFTER UPDATE OF status,service_suspended ON users
    FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status OR OLD.service_suspended IS DISTINCT FROM NEW.service_suspended)
    EXECUTE FUNCTION wake_user_subscription_connector();
