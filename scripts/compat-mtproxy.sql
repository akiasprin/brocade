-- Additive MTProxy compatibility for an existing Console database.
-- Run only while every Console writer is stopped. Back up first and update the SQLx checksum only
-- after the resulting columns, defaults and constraints match the target 0001 schema.
--
-- psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade -f scripts/compat-mtproxy.sql
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL ROLE :"app_role";
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
SELECT pg_advisory_xact_lock(1481785689);

-- Deliberately omit IF NOT EXISTS: a repeated or partially compatible run must stop for review.
ALTER TABLE control_state
    ADD COLUMN port_mtproto_base INTEGER DEFAULT 28800 NOT NULL;
ALTER TABLE control_state
    ADD CONSTRAINT control_state_port_mtproto_base_range
    CHECK (port_mtproto_base BETWEEN 1 AND 65535);

ALTER TABLE ingresses ADD COLUMN mtproto_port INTEGER;
ALTER TABLE ingresses
    ADD CONSTRAINT ingresses_mtproto_port_range
    CHECK (mtproto_port IS NULL OR mtproto_port BETWEEN 1 AND 65535);
ALTER TABLE ingresses
    ADD CONSTRAINT ingresses_mtproto_vless_port_distinct
    CHECK (mtproto_port IS NULL OR transport_kind IS NULL OR mtproto_port <> port);
ALTER TABLE ingresses
    ADD CONSTRAINT ingresses_mtproto_anytls_port_distinct
    CHECK (mtproto_port IS NULL OR anytls_port IS NULL OR mtproto_port <> anytls_port);
ALTER TABLE ingresses
    ADD CONSTRAINT ingresses_mtproto_vless_encryption_port_distinct
    CHECK (
        mtproto_port IS NULL
        OR vless_encryption_port IS NULL
        OR mtproto_port <> vless_encryption_port
    );
ALTER TABLE ingresses DROP CONSTRAINT ingresses_has_a_wire;
ALTER TABLE ingresses
    ADD CONSTRAINT ingresses_has_a_wire CHECK (
        transport_kind IS NOT NULL
        OR anytls_enabled
        OR hy2_enabled
        OR vless_encryption_port IS NOT NULL
        OR mtproto_port IS NOT NULL
    );

ALTER TABLE user_online_sources DROP CONSTRAINT user_online_sources_protocols_shape;
ALTER TABLE user_online_sources
    ADD CONSTRAINT user_online_sources_protocols_shape CHECK (
        protocols IS NULL OR CASE WHEN jsonb_typeof(protocols) = 'array' THEN
            jsonb_array_length(protocols) BETWEEN 1 AND 5
            AND protocols <@ '["vless", "anytls", "hysteria2", "mtproto", "unknown"]'::jsonb
        ELSE FALSE END
    );
COMMIT;
