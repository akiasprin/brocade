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
