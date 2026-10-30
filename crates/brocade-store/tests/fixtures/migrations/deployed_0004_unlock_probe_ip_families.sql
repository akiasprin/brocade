-- Legacy rows remain automatic; independent jobs use distinct scope keys.
ALTER TABLE unlock_probe_jobs ADD COLUMN address_family TEXT CHECK (address_family IN ('v4','v6'));
CREATE INDEX unlock_probe_jobs_family_worker ON unlock_probe_jobs(node_id,address_family,state,created_at);
