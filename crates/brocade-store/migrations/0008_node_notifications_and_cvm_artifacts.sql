-- Node notification preferences are new in 0008; published 0007 remains unchanged.
-- IF NOT EXISTS only accommodates the exact, separately verified deployed-0007 conversion.
CREATE TABLE IF NOT EXISTS node_notification_preferences (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    public_ip_changes BOOLEAN NOT NULL DEFAULT TRUE,
    changed_by TEXT NOT NULL CHECK (length(btrim(changed_by)) BETWEEN 1 AND 256),
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Never silently accept a same-name table with a different shape.
DO $migration$
BEGIN
    IF (SELECT jsonb_agg(jsonb_build_array(a.attname,format_type(a.atttypid,a.atttypmod),a.attnotnull,pg_get_expr(d.adbin,d.adrelid)) ORDER BY a.attnum)
        FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
        WHERE a.attrelid='public.node_notification_preferences'::regclass AND a.attnum>0 AND NOT a.attisdropped) IS DISTINCT FROM
        '[["node_id","text",true,null],["public_ip_changes","boolean",true,"true"],["changed_by","text",true,null],["changed_at","timestamp with time zone",true,"now()"]]'::jsonb
       OR (SELECT jsonb_agg(jsonb_build_array(k.conname,pg_get_constraintdef(k.oid),k.convalidated) ORDER BY k.conname)
        FROM pg_constraint k WHERE k.conrelid='public.node_notification_preferences'::regclass AND k.contype IN ('c','f','p')) IS DISTINCT FROM
        '[["node_notification_preferences_changed_by_check","CHECK (((length(btrim(changed_by)) >= 1) AND (length(btrim(changed_by)) <= 256)))",true],["node_notification_preferences_node_id_fkey","FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE",true],["node_notification_preferences_pkey","PRIMARY KEY (node_id)",true]]'::jsonb
       OR (SELECT jsonb_agg(jsonb_build_array(pg_get_indexdef(i.indexrelid),i.indisvalid,i.indisready) ORDER BY pg_get_indexdef(i.indexrelid))
        FROM pg_index i WHERE i.indrelid='public.node_notification_preferences'::regclass) IS DISTINCT FROM
        '[["CREATE UNIQUE INDEX node_notification_preferences_pkey ON public.node_notification_preferences USING btree (node_id)",true,true]]'::jsonb
       OR EXISTS (SELECT 1 FROM pg_trigger t WHERE t.tgrelid='public.node_notification_preferences'::regclass AND NOT t.tgisinternal) THEN
        RAISE EXCEPTION 'node_notification_preferences does not match the verified 0008 schema';
    END IF;
END $migration$;
-- Native Cube products are keyed by immutable execution generation, mapped to global revisions below.
CREATE TABLE cvm_image_manifests (
    sha256 TEXT PRIMARY KEY CHECK (sha256 ~ '^[a-f0-9]{64}$'),
    content TEXT NOT NULL CHECK (octet_length(content)<=2097152),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER immutable_cvm_image_manifest BEFORE UPDATE OR DELETE ON cvm_image_manifests
    FOR EACH ROW EXECUTE FUNCTION immutable_cvm_spec();
CREATE TABLE cvm_spec_artifact_sets (
    instance_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    compiler TEXT NOT NULL,
    input_sha256 TEXT NOT NULL CHECK (input_sha256 ~ '^[a-f0-9]{64}$'),
    spec_sha256 TEXT NOT NULL CHECK (spec_sha256 ~ '^[a-f0-9]{64}$'),
    input JSONB NOT NULL,
    PRIMARY KEY(instance_id,generation),
    FOREIGN KEY(instance_id,generation) REFERENCES cvm_spec_versions(instance_id,generation)
);
CREATE TRIGGER immutable_cvm_spec_artifact_set BEFORE UPDATE OR DELETE ON cvm_spec_artifact_sets
    FOR EACH ROW EXECUTE FUNCTION immutable_cvm_spec();
CREATE TABLE cvm_spec_artifacts (
    instance_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('cube/sandbox.json','cube/egress.json','cube/attachments.json','guest/init.json')),
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[a-f0-9]{64}$'),
    content TEXT NOT NULL CHECK (octet_length(content)<=4194304),
    PRIMARY KEY(instance_id,generation,kind),
    FOREIGN KEY(instance_id,generation) REFERENCES cvm_spec_artifact_sets(instance_id,generation)
);
CREATE TRIGGER immutable_cvm_spec_artifact BEFORE UPDATE OR DELETE ON cvm_spec_artifacts
    FOR EACH ROW EXECUTE FUNCTION immutable_cvm_spec();

-- CVM intent shares the global revision identity. Generation is execution fencing only.
-- Network credentials are sealed with the node/revision as authenticated context.
CREATE TABLE cvm_revision_nodes (
    revision_id BIGINT NOT NULL REFERENCES revisions(id),
    node_id TEXT NOT NULL,
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[a-f0-9]{64}$'),
    payload TEXT NOT NULL,
    PRIMARY KEY (revision_id,node_id)
);
CREATE TRIGGER immutable_cvm_revision BEFORE UPDATE OR DELETE ON cvm_revision_nodes
    FOR EACH ROW EXECUTE FUNCTION immutable_cvm_spec();
CREATE TABLE cvm_serving_nodes (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    revision_id BIGINT NOT NULL REFERENCES revisions(id),
    payload TEXT NOT NULL,
    entry_generation BIGINT NOT NULL DEFAULT 0,
    entry_sha256 TEXT,
    entry_ids TEXT[] NOT NULL DEFAULT '{}',
    tunnel_ids TEXT[] NOT NULL DEFAULT '{}'
);
ALTER TABLE deployment_target_state ADD COLUMN cvm_revision_id BIGINT REFERENCES revisions(id);
ALTER TABLE deployment_target_state ADD COLUMN cvm_machine_ready BOOLEAN NOT NULL DEFAULT FALSE;
CREATE TABLE cvm_revision_instances (
    instance_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    revision_id BIGINT NOT NULL REFERENCES revisions(id),
    PRIMARY KEY(instance_id,generation),
    FOREIGN KEY(instance_id,generation) REFERENCES cvm_spec_versions(instance_id,generation)
);
CREATE TRIGGER immutable_cvm_revision_instance BEFORE UPDATE OR DELETE ON cvm_revision_instances
    FOR EACH ROW EXECUTE FUNCTION immutable_cvm_spec();
ALTER TABLE cvm_nic_nodes ADD COLUMN report_received_at TIMESTAMPTZ;
CREATE TABLE cvm_revision_bindings (
    binding_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    revision_id BIGINT NOT NULL REFERENCES revisions(id),
    PRIMARY KEY(binding_id,generation)
);
CREATE TRIGGER immutable_cvm_revision_binding BEFORE UPDATE OR DELETE ON cvm_revision_bindings
    FOR EACH ROW EXECUTE FUNCTION immutable_cvm_spec();
