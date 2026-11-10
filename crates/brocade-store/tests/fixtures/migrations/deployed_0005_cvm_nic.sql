-- Keep the Xray service intent in immutable model revisions; historical snapshots default on.
ALTER TABLE nodes ADD COLUMN xray_enabled BOOLEAN NOT NULL DEFAULT TRUE;

CREATE FUNCTION node_xray_forwarding_transition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND (OLD.xray_state = 'disabled') = (NEW.xray_state = 'disabled') THEN
        RETURN NEW;
    END IF;
    INSERT INTO forwarding_node_state (node_id, desired_generation, reconcile_status)
    VALUES (NEW.node_id, 1, 'pending')
    ON CONFLICT (node_id) DO UPDATE SET
        desired_generation = forwarding_node_state.desired_generation + 1,
        reconcile_status = 'pending', last_error = NULL;
    RETURN NEW;
END
$$;
CREATE TRIGGER node_xray_forwarding_transition AFTER INSERT OR UPDATE OF xray_state ON node_applied_state
    FOR EACH ROW EXECUTE FUNCTION node_xray_forwarding_transition();

CREATE TRIGGER agent_channel_xray_forwarding AFTER INSERT OR UPDATE OF desired_generation ON forwarding_node_state
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('desired', 'node_id');

-- Freeze installation-only approval so a later crash cannot masquerade as an intentional stop.
ALTER TABLE binary_release_targets ADD COLUMN xray_service_was_disabled BOOLEAN NOT NULL DEFAULT FALSE;

-- CVM lifecycle and resource ownership
-- Operational instance intent is separate from the proxy model's publication domain.
CREATE TABLE cvm_hosts (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    capabilities JSONB NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    error TEXT CHECK (length(error) <= 500)
);
-- User-host network quotas. Capacity holds outlive a pending downsize until
-- the node confirms the new revision and the old states have drained.
CREATE TABLE cvm_network_quota_hosts (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE RESTRICT,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    capacity JSONB NOT NULL
);
CREATE TABLE cvm_user_host_network_quotas (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    id TEXT NOT NULL UNIQUE CHECK (id ~ '^[0-9a-f]{16}$' AND id <> '0000000000000000'),
    revision BIGINT NOT NULL CHECK (revision > 0),
    limits JSONB NOT NULL,
    held_states BIGINT NOT NULL CHECK (held_states >= 0),
    report JSONB,
    reported_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,user_id,node_id),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE RESTRICT
);
CREATE INDEX cvm_user_host_network_quotas_node ON cvm_user_host_network_quotas(node_id,id);
CREATE TRIGGER agent_channel_cvm_network_quota AFTER INSERT OR UPDATE OF revision,limits
ON cvm_user_host_network_quotas FOR EACH ROW
EXECUTE FUNCTION notify_agent_channel_change('desired','node_id');
CREATE TABLE cvm_network_quota_events (
    sequence BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE cvm_quotas (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    allowed_nodes JSONB NOT NULL DEFAULT '[]',
    max_instances INTEGER NOT NULL DEFAULT 0 CHECK (max_instances BETWEEN 0 AND 256),
    max_vcpus BIGINT NOT NULL DEFAULT 0 CHECK (max_vcpus >= 0),
    max_memory_bytes BIGINT NOT NULL DEFAULT 0 CHECK (max_memory_bytes >= 0),
    max_disk_bytes BIGINT NOT NULL DEFAULT 0 CHECK (max_disk_bytes >= 0),
    max_interfaces INTEGER NOT NULL DEFAULT 0 CHECK (max_interfaces BETWEEN 0 AND 4096),
    PRIMARY KEY (tenant_id,user_id),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE RESTRICT,
    CHECK (jsonb_typeof(allowed_nodes) = 'array' AND jsonb_array_length(allowed_nodes) <= 64)
);
CREATE TABLE cvm_instances (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    generation BIGINT NOT NULL CHECK (generation > 0),
    desired JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','applied','failed','unknown','restart_required')),
    report JSONB,
    reserved BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE RESTRICT
);
CREATE INDEX cvm_instances_machine ON cvm_instances(node_id,id);
CREATE INDEX cvm_instances_owner ON cvm_instances(tenant_id,user_id,id);
CREATE FUNCTION cvm_network_memory_budget_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='INSERT' THEN
        UPDATE cvm_network_quota_hosts SET revision=revision+1 WHERE node_id=NEW.node_id;
    ELSIF NEW.reserved IS DISTINCT FROM OLD.reserved OR
          NEW.desired#>'{spec,resources,memory_bytes}' IS DISTINCT FROM OLD.desired#>'{spec,resources,memory_bytes}' THEN
        UPDATE cvm_network_quota_hosts SET revision=revision+1 WHERE node_id=NEW.node_id;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER cvm_network_memory_budget_changed AFTER INSERT OR UPDATE OF desired,reserved
ON cvm_instances FOR EACH ROW EXECUTE FUNCTION cvm_network_memory_budget_changed();
CREATE TABLE cvm_volumes (
    instance_id TEXT PRIMARY KEY REFERENCES cvm_instances(id) ON DELETE RESTRICT,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    bytes BIGINT NOT NULL CHECK (bytes > 0),
    backend_id TEXT,
    state TEXT NOT NULL DEFAULT 'reserved' CHECK (state IN ('reserved','present','retained','released')),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE RESTRICT
);
CREATE TABLE cvm_interfaces (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE RESTRICT,
    source_id TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('contact','business')),
    desired_attached BOOLEAN NOT NULL DEFAULT TRUE,
    detached_confirmed BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE INDEX cvm_interfaces_instance ON cvm_interfaces(instance_id,id);
CREATE TABLE cvm_spec_versions (
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE RESTRICT,
    generation BIGINT NOT NULL,
    spec JSONB NOT NULL,
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[a-f0-9]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (instance_id,generation)
);
CREATE TABLE cvm_operations (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE RESTRICT,
    generation BIGINT NOT NULL,
    kind TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    request_sha256 TEXT NOT NULL CHECK (request_sha256 ~ '^[a-f0-9]{64}$'),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','applied','failed','unknown','restart_required')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (actor_id,request_id),
    UNIQUE (instance_id,generation)
);
CREATE FUNCTION immutable_cvm_spec() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'CVM specification versions are immutable';
END
$$;
CREATE TRIGGER immutable_cvm_spec BEFORE UPDATE OR DELETE ON cvm_spec_versions
    FOR EACH ROW EXECUTE FUNCTION immutable_cvm_spec();
CREATE TRIGGER agent_channel_cvm_desired AFTER INSERT OR UPDATE OF generation ON cvm_instances
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('desired','node_id');
CREATE TABLE cvm_quota_changes (
    id BIGSERIAL PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE RESTRICT
);
CREATE INDEX cvm_quota_changes_owner ON cvm_quota_changes(tenant_id,user_id,id DESC);

-- Independent CVM observations
-- Observation history is independent of lifecycle receipts and immutable intent.
CREATE TABLE cvm_observation_state (
    instance_id TEXT PRIMARY KEY REFERENCES cvm_instances(id) ON DELETE CASCADE,
    observed_at BIGINT NOT NULL CHECK (observed_at > 0),
    observation JSONB NOT NULL CHECK (jsonb_typeof(observation)='object'),
    previous_metrics JSONB,
    latest_sample JSONB,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE cvm_observation_samples (
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE CASCADE,
    bucket_millis BIGINT NOT NULL CHECK (bucket_millis >= 0 AND bucket_millis % 60000 = 0),
    boot_id TEXT NOT NULL CHECK (length(boot_id)=36),
    observed_at BIGINT NOT NULL CHECK (observed_at > 0),
    weight_millis BIGINT NOT NULL CHECK (weight_millis > 0),
    sample JSONB NOT NULL CHECK (jsonb_typeof(sample)='object'),
    PRIMARY KEY (instance_id,bucket_millis,boot_id)
);
CREATE INDEX cvm_observation_samples_expiry ON cvm_observation_samples(observed_at);
CREATE TABLE cvm_observation_events (
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE CASCADE,
    observed_at BIGINT NOT NULL CHECK (observed_at > 0),
    kind TEXT NOT NULL CHECK (kind IN ('reboot','oom','guest_unreachable','guest_recovered','service_unreachable','service_recovered')),
    detail TEXT NOT NULL CHECK (length(detail) <= 300),
    PRIMARY KEY (instance_id,observed_at,kind)
);
CREATE INDEX cvm_observation_events_expiry ON cvm_observation_events(observed_at);

-- Console-owned CVM terminal policy
-- Administrative access policy is not an Agent-owned capability observation.
CREATE TABLE cvm_terminal_policies (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT false,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    actor_id TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE cvm_terminal_policy_events (
    node_id TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    enabled BOOLEAN NOT NULL,
    actor_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (node_id, revision)
);

-- Reusable container NIC services. Allocations remain owned until both endpoints
-- have proved cleanup; publication admission does not revoke existing devices.
CREATE TABLE cvm_nic_nodes (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE RESTRICT,
    capabilities JSONB NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    desired_generation BIGINT NOT NULL DEFAULT 1,
    report JSONB
);
CREATE TRIGGER agent_channel_cvm_nic AFTER INSERT OR UPDATE OF desired_generation ON cvm_nic_nodes
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('desired', 'node_id');
CREATE TABLE cvm_nic_publications (
    id TEXT PRIMARY KEY,
    provider_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    revision BIGINT NOT NULL CHECK (revision > 0),
    admitting BOOLEAN NOT NULL DEFAULT TRUE,
    deleted BOOLEAN NOT NULL DEFAULT FALSE,
    spec JSONB NOT NULL,
    actor_id TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE cvm_nic_publication_versions (
    publication_id TEXT NOT NULL REFERENCES cvm_nic_publications(id) ON DELETE RESTRICT,
    revision BIGINT NOT NULL,
    spec JSONB NOT NULL,
    actor_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (publication_id, revision)
);
CREATE TRIGGER immutable_cvm_nic_publication BEFORE UPDATE OR DELETE ON cvm_nic_publication_versions
    FOR EACH ROW EXECUTE FUNCTION immutable_cvm_spec();
CREATE TABLE cvm_nic_requests (
    actor_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    digest TEXT NOT NULL,
    publication_id TEXT,
    result JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (actor_id, request_id)
);
CREATE TABLE cvm_nic_allocations (
    id TEXT PRIMARY KEY,
    interface_id TEXT NOT NULL REFERENCES cvm_interfaces(id) ON DELETE RESTRICT,
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE RESTRICT,
    publication_id TEXT NOT NULL REFERENCES cvm_nic_publications(id) ON DELETE RESTRICT,
    provider_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    consumer_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    spec JSONB NOT NULL,
    released BOOLEAN NOT NULL DEFAULT FALSE,
    retired BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX cvm_nic_current_allocation ON cvm_nic_allocations(interface_id) WHERE NOT released;
CREATE TABLE cvm_nic_resources (
    id BIGSERIAL PRIMARY KEY,
    allocation_id TEXT NOT NULL REFERENCES cvm_nic_allocations(id) ON DELETE RESTRICT,
    binding_id TEXT,
    scope TEXT NOT NULL,
    kind TEXT NOT NULL,
    value TEXT NOT NULL,
    released BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE UNIQUE INDEX cvm_nic_resource_ownership ON cvm_nic_resources(scope, kind, value) WHERE NOT released;
CREATE TABLE cvm_nic_bindings (
    id TEXT PRIMARY KEY,
    interface_id TEXT NOT NULL REFERENCES cvm_interfaces(id) ON DELETE RESTRICT,
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE RESTRICT,
    allocation_id TEXT NOT NULL REFERENCES cvm_nic_allocations(id) ON DELETE RESTRICT,
    provider_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    consumer_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE RESTRICT,
    generation BIGINT NOT NULL CHECK (generation > 0),
    phase TEXT NOT NULL CHECK (phase IN ('preparing','active','standby','withdrawing')),
    target JSONB NOT NULL,
    sealed TEXT NOT NULL,
    consumer_report JSONB,
    provider_report JSONB,
    last_error TEXT,
    cleaned BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX cvm_nic_bindings_participants ON cvm_nic_bindings(provider_node, consumer_node) WHERE NOT cleaned;
CREATE TABLE cvm_nic_route_versions (
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE RESTRICT,
    revision BIGINT NOT NULL,
    routes JSONB NOT NULL,
    actor_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (instance_id, revision)
);
CREATE TRIGGER immutable_cvm_nic_routes BEFORE UPDATE OR DELETE ON cvm_nic_route_versions
    FOR EACH ROW EXECUTE FUNCTION immutable_cvm_spec();
CREATE TABLE cvm_nic_ingress (
    interface_id TEXT PRIMARY KEY REFERENCES cvm_interfaces(id) ON DELETE RESTRICT,
    revision BIGINT NOT NULL DEFAULT 0,
    rules JSONB NOT NULL DEFAULT '[]'
);

-- NIC business IP usage has one accounting boundary, the consumer's owned LAN.
CREATE TABLE cvm_nic_meter_cursors (
    interface_id TEXT NOT NULL REFERENCES cvm_interfaces(id) ON DELETE RESTRICT,
    epoch TEXT NOT NULL CHECK (epoch ~ '^[0-9a-f]{64}$'),
    observed_at BIGINT NOT NULL CHECK (observed_at > 0),
    uploaded BIGINT NOT NULL CHECK (uploaded >= 0),
    downloaded BIGINT NOT NULL CHECK (downloaded >= 0),
    PRIMARY KEY (interface_id,epoch)
);
CREATE TABLE cvm_nic_monthly_usage (
    interface_id TEXT NOT NULL REFERENCES cvm_interfaces(id) ON DELETE RESTRICT,
    period DATE NOT NULL,
    uploaded BIGINT NOT NULL DEFAULT 0 CHECK (uploaded >= 0),
    downloaded BIGINT NOT NULL DEFAULT 0 CHECK (downloaded >= 0),
    coverage_gap BOOLEAN NOT NULL DEFAULT FALSE,
    observed_at BIGINT NOT NULL,
    PRIMARY KEY (interface_id,period)
);

-- One source for managed static listener ranges and NIC/forwarding conflicts.
CREATE VIEW managed_static_listener_ranges AS
SELECT node_id,protocol,port_start,port_end FROM (
    SELECT n.id AS node_id,p.protocol,p.port_start,p.port_end
    FROM nodes n CROSS JOIN LATERAL (VALUES
        ('tcp',n.api_port,n.api_port),
        ('udp',n.wg_listen_port,n.wg_listen_port),
        ('tcp',CASE WHEN n.wg_transport->>'t'='fake_tcp' THEN (n.wg_transport#>>'{v,port}')::integer END,
               CASE WHEN n.wg_transport->>'t'='fake_tcp' THEN (n.wg_transport#>>'{v,port}')::integer END)
    ) p(protocol,port_start,port_end)
    UNION ALL
    SELECT i.node_id,p.protocol,p.port_start,p.port_end
    FROM ingresses i CROSS JOIN LATERAL (VALUES
        ('tcp',CASE WHEN i.transport_kind IS NOT NULL THEN i.port END,CASE WHEN i.transport_kind IS NOT NULL THEN i.port END),
        ('tcp',i.vless_encryption_port,i.vless_encryption_port),
        ('tcp',CASE WHEN i.anytls_enabled THEN i.anytls_port END,CASE WHEN i.anytls_enabled THEN i.anytls_port END),
        ('tcp',i.mtproto_port,i.mtproto_port),
        ('tcp',i.xhttp_download_v4_origin_port,i.xhttp_download_v4_origin_port),
        ('tcp',i.xhttp_download_v6_origin_port,i.xhttp_download_v6_origin_port),
        ('udp',CASE WHEN i.hy2_enabled THEN coalesce(i.hy2_hop_start,i.hy2_port) END,
               CASE WHEN i.hy2_enabled THEN coalesce(i.hy2_hop_end,i.hy2_port) END)
    ) p(protocol,port_start,port_end)
    UNION ALL SELECT node_id,'tcp',hop_in_port,hop_in_port FROM steps
) ranges WHERE port_start IS NOT NULL AND port_start>0 AND port_end>=port_start;

ALTER TABLE cvm_nic_bindings ADD COLUMN consumer_reported_at TIMESTAMPTZ;
ALTER TABLE cvm_nic_bindings ADD COLUMN provider_reported_at TIMESTAMPTZ;
