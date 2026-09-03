-- AnyTLS is an independent TCP listener and therefore cannot be represented by the historical
-- single `transport_kind` column when VLESS and AnyTLS are enabled together.
--
-- Keep this change separate from 0001_init.sql: SQLx records each migration checksum, and the
-- initial migration is already present in deployed databases.

ALTER TABLE ingresses
    ADD COLUMN IF NOT EXISTS anytls_enabled BOOLEAN DEFAULT FALSE NOT NULL,
    ADD COLUMN IF NOT EXISTS anytls_port INTEGER,
    ADD COLUMN IF NOT EXISTS anytls_padding_scheme JSONB DEFAULT '[]'::jsonb NOT NULL,
    ADD COLUMN IF NOT EXISTS anytls_masquerade_kind TEXT DEFAULT '404' NOT NULL,
    ADD COLUMN IF NOT EXISTS anytls_masquerade_content TEXT DEFAULT '' NOT NULL,
    ADD COLUMN IF NOT EXISTS anytls_masquerade_headers JSONB DEFAULT '{}'::jsonb NOT NULL,
    ADD COLUMN IF NOT EXISTS anytls_masquerade_status_code INTEGER DEFAULT 200 NOT NULL;

ALTER TABLE ingresses
    DROP CONSTRAINT IF EXISTS ingresses_anytls_port_present,
    DROP CONSTRAINT IF EXISTS ingresses_anytls_port_range,
    DROP CONSTRAINT IF EXISTS ingresses_anytls_port_distinct,
    DROP CONSTRAINT IF EXISTS ingresses_anytls_padding_scheme_check,
    DROP CONSTRAINT IF EXISTS ingresses_anytls_masquerade_kind_check,
    DROP CONSTRAINT IF EXISTS ingresses_anytls_masquerade_headers_check,
    DROP CONSTRAINT IF EXISTS ingresses_anytls_masquerade_status_check,
    DROP CONSTRAINT IF EXISTS ingresses_has_a_wire;

ALTER TABLE ingresses
    ADD CONSTRAINT ingresses_anytls_port_present CHECK ((anytls_port IS NOT NULL) = anytls_enabled),
    ADD CONSTRAINT ingresses_anytls_port_range CHECK (anytls_port IS NULL OR (anytls_port BETWEEN 1 AND 65535)),
    ADD CONSTRAINT ingresses_anytls_port_distinct CHECK (
        anytls_port IS NULL OR transport_kind IS NULL OR anytls_port <> port
    ),
    ADD CONSTRAINT ingresses_anytls_padding_scheme_check CHECK (
        jsonb_typeof(anytls_padding_scheme) = 'array'
    ),
    ADD CONSTRAINT ingresses_anytls_masquerade_kind_check CHECK (anytls_masquerade_kind IN ('404', 'string')),
    ADD CONSTRAINT ingresses_anytls_masquerade_headers_check CHECK (jsonb_typeof(anytls_masquerade_headers) = 'object'),
    ADD CONSTRAINT ingresses_anytls_masquerade_status_check CHECK (
        anytls_masquerade_kind <> 'string'
        OR anytls_masquerade_status_code BETWEEN 200 AND 599
    ),
    ADD CONSTRAINT ingresses_has_a_wire CHECK ((transport_kind IS NOT NULL OR anytls_enabled OR hy2_enabled));
