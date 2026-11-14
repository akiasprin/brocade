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
