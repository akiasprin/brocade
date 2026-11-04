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
