-- This is the destructive boundary for the unreleased CVM data model. Published
-- non-CVM state and migration receipts remain intact, while every CVM row created
-- by 0005-0008 is discarded instead of being interpreted as current intent.
UPDATE deployment_target_state
SET cvm_revision_id = NULL,
    cvm_machine_ready = FALSE,
    desired_structure = desired_structure - 'cvm',
    observed_after = observed_after - 'cvm',
    verdict = verdict - 'cvm_pending' - 'cvm_status'
WHERE cvm_revision_id IS NOT NULL
   OR cvm_machine_ready
   OR desired_structure ? 'cvm'
   OR coalesce(observed_after, '{}'::jsonb) ? 'cvm'
   OR coalesce(verdict, '{}'::jsonb) ? 'cvm_pending'
   OR coalesce(verdict, '{}'::jsonb) ? 'cvm_status';

TRUNCATE TABLE
    cvm_host_policy_events,
    cvm_host_policies,
    cvm_nic_monthly_usage,
    cvm_nic_meter_cursors,
    cvm_nic_route_versions,
    cvm_nic_bindings,
    cvm_nic_resources,
    cvm_entries,
    cvm_entry_tunnels,
    cvm_entry_publication_versions,
    cvm_entry_publications,
    cvm_entry_nodes,
    cvm_nic_allocations,
    cvm_nic_requests,
    cvm_nic_publication_versions,
    cvm_nic_publications,
    cvm_nic_nodes,
    cvm_terminal_policy_events,
    cvm_terminal_policies,
    cvm_observation_events,
    cvm_observation_samples,
    cvm_observation_state,
    cvm_quota_changes,
    cvm_operations,
    cvm_spec_artifacts,
    cvm_spec_artifact_sets,
    cvm_revision_bindings,
    cvm_revision_instances,
    cvm_serving_nodes,
    cvm_revision_nodes,
    cvm_image_manifests,
    cvm_spec_versions,
    cvm_interfaces,
    cvm_volumes,
    cvm_instances,
    cvm_quotas,
    cvm_network_quota_events,
    cvm_user_host_network_quotas,
    cvm_network_quota_hosts,
    cvm_hosts
RESTART IDENTITY;

-- Published migrations are immutable. The current model assigns instances through
-- explicit access events and does not import any pre-0009 ownership or credentials.
ALTER TABLE cvm_instances ALTER COLUMN user_id DROP NOT NULL;
ALTER TABLE cvm_volumes ALTER COLUMN user_id DROP NOT NULL;

CREATE TABLE cvm_instance_access_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE RESTRICT,
    tenant_id TEXT NOT NULL,
    previous_user_id TEXT,
    user_id TEXT,
    actor_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX cvm_instance_access_generation ON cvm_instance_access_events(instance_id,generation);

-- Recoverable passwords are sealed with the instance and revision as AEAD context.
-- They never belong in immutable specifications, artifacts or operation requests.
CREATE TABLE cvm_guest_credentials (
    instance_id TEXT NOT NULL REFERENCES cvm_instances(id) ON DELETE RESTRICT,
    revision BIGINT NOT NULL CHECK (revision > 0),
    password_sealed TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (instance_id,revision)
);

ALTER TABLE cvm_network_quota_hosts ADD COLUMN report JSONB;
ALTER TABLE cvm_network_quota_hosts ADD COLUMN reported_at TIMESTAMPTZ;

-- The node now receives one host-wide assignment, independent of user identity.
-- The destructive boundary above already discarded the old admission ledgers.
DROP TABLE cvm_user_host_network_quotas;
DROP TABLE cvm_quotas;

-- A permanently removed terminal machine must not be kept alive by immutable CVM execution
-- evidence. Historical rows retain the original machine ID as text; live admission and
-- retirement code still validate the machine before creating or changing resources.
ALTER TABLE cvm_instances
    DROP CONSTRAINT cvm_instances_node_id_fkey;
ALTER TABLE cvm_nic_publications
    DROP CONSTRAINT cvm_nic_publications_provider_node_fkey;
ALTER TABLE cvm_nic_allocations
    DROP CONSTRAINT cvm_nic_allocations_provider_node_fkey,
    DROP CONSTRAINT cvm_nic_allocations_consumer_node_fkey;
ALTER TABLE cvm_nic_bindings
    DROP CONSTRAINT cvm_nic_bindings_provider_node_fkey,
    DROP CONSTRAINT cvm_nic_bindings_consumer_node_fkey;
ALTER TABLE cvm_entry_publications
    DROP CONSTRAINT cvm_entry_publications_provider_node_fkey;
ALTER TABLE cvm_entry_tunnels
    DROP CONSTRAINT cvm_entry_tunnels_entry_node_fkey,
    DROP CONSTRAINT cvm_entry_tunnels_target_node_fkey;
ALTER TABLE cvm_entries
    DROP CONSTRAINT cvm_entries_provider_node_fkey;
