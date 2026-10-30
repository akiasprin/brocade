-- Live diagnostic policy: independent of model revisions and node configuration.
ALTER TABLE control_state ADD COLUMN unlock_refresh_stale BOOLEAN NOT NULL DEFAULT TRUE;
UPDATE unlock_probe_discovery SET next_discovery_at = LEAST(next_discovery_at, now());
