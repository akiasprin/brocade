-- Node notification preferences remain in the immutable published 0007.
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
