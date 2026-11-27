-- Template for prepare-cvm-stack-0009-compat.py; do not execute directly.
-- Rollback archive SHA-256 (restored and verified before preparation): @BACKUP_SHA256@
\set ON_ERROR_STOP on
\set VERBOSITY terse
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '120s';
SET LOCAL search_path = pg_catalog, pg_temp;
SET LOCAL timezone = 'UTC';

-- Keep Console and all other writers stopped through the cutover.
LOCK TABLE public._sqlx_migrations, public.cvm_stack_releases,
    public.cvm_stack_release_events, public.cvm_stack_observations IN ACCESS EXCLUSIVE MODE;

CREATE TEMP TABLE expected_receipts (version bigint PRIMARY KEY, description text NOT NULL, checksum bytea NOT NULL) ON COMMIT DROP;
INSERT INTO expected_receipts VALUES
    @HISTORY@;

-- The migration's own definitions are the reference on this PostgreSQL major.
-- Compare names, types, default/identity behavior, constraints, indexes and
-- triggers without depending on cross-version deparser formatting or OIDs.
CREATE TEMP TABLE nodes (id text PRIMARY KEY) ON COMMIT DROP;
CREATE TEMP TABLE _sqlx_migrations (
    version bigint PRIMARY KEY,
    description text NOT NULL,
    installed_on timestamptz NOT NULL DEFAULT now(),
    success boolean NOT NULL,
    checksum bytea NOT NULL,
    execution_time bigint NOT NULL
) ON COMMIT DROP;

CREATE FUNCTION pg_temp.stack_schema(namespace_oid oid, persistence "char") RETURNS jsonb
LANGUAGE sql STABLE AS $schema$
    SELECT jsonb_object_agg(c.relname, jsonb_build_object(
        'properties', ARRAY[c.relkind = 'r', c.relpersistence = persistence,
            c.relowner = current_user::regrole, NOT c.relrowsecurity, NOT c.relforcerowsecurity,
            NOT c.relhasrules, NOT EXISTS (SELECT FROM pg_inherits h WHERE c.oid IN (h.inhrelid, h.inhparent))],
        'options', c.reloptions,
        'columns', (SELECT jsonb_agg(jsonb_build_array(a.attname, format_type(a.atttypid, a.atttypmod),
            a.attnotnull, a.attidentity, a.attgenerated, a.attcollation::regcollation::text,
            pg_get_expr(d.adbin, d.adrelid),
            (SELECT jsonb_build_array(s.relname, s.relowner = c.relowner, s.relpersistence = persistence,
                 q.seqtypid::regtype::text, q.seqstart, q.seqincrement, q.seqmax, q.seqmin, q.seqcache, q.seqcycle)
             FROM pg_depend dep JOIN pg_class s ON s.oid = dep.objid
             JOIN pg_sequence q ON q.seqrelid = s.oid
             WHERE dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass
               AND dep.refobjid = c.oid AND dep.refobjsubid = a.attnum AND dep.deptype = 'i')) ORDER BY a.attnum)
            FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
            WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped),
        'constraints', (SELECT jsonb_agg(jsonb_build_array(k.conname, k.convalidated, k.condeferrable,
            k.condeferred, replace(pg_get_constraintdef(k.oid), 'REFERENCES ' || quote_ident(n.nspname) || '.', 'REFERENCES ')) ORDER BY k.conname)
            FROM pg_constraint k WHERE k.conrelid = c.oid),
        -- PostgreSQL uses the session alias pg_temp when deparsing temp table indexes.
        'indexes', (SELECT jsonb_agg(jsonb_build_array(replace(pg_get_indexdef(i.indexrelid),
            ' ON ' || CASE WHEN n.oid = pg_my_temp_schema() THEN 'pg_temp' ELSE quote_ident(n.nspname) END || '.', ' ON '),
            i.indisvalid, i.indisready, i.indislive) ORDER BY ix.relname)
            FROM pg_index i JOIN pg_class ix ON ix.oid = i.indexrelid WHERE i.indrelid = c.oid),
        'triggers', (SELECT jsonb_agg(jsonb_build_array(t.tgname, t.tgenabled, pg_get_triggerdef(t.oid)) ORDER BY t.tgname)
            FROM pg_trigger t WHERE t.tgrelid = c.oid AND NOT t.tgisinternal)
    ))
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relnamespace = namespace_oid AND c.relname IN (
        '_sqlx_migrations', 'cvm_stack_observations', 'cvm_stack_releases', 'cvm_stack_release_events'
    );
$schema$;

@OLD_SCHEMA@

DO $guard$
BEGIN
    IF pg_temp.stack_schema('public'::regnamespace, 'p') IS DISTINCT FROM pg_temp.stack_schema(pg_my_temp_schema(), 't') THEN
        RAISE EXCEPTION 'unexpected prerelease Cube Stack schema or owner';
    END IF;
    IF EXISTS (SELECT FROM expected_receipts e FULL JOIN public._sqlx_migrations m USING (version)
               WHERE e.checksum IS DISTINCT FROM m.checksum OR e.description IS DISTINCT FROM m.description
                  OR m.success IS DISTINCT FROM true) THEN
        RAISE EXCEPTION 'unrecognized prerelease Cube Stack migration history';
    END IF;
    IF EXISTS (SELECT FROM public.cvm_stack_releases WHERE status NOT IN ('succeeded', 'failed', 'recovered', 'cancelled')) THEN
        RAISE EXCEPTION 'live legacy Cube Stack release remains; settle or cancel before cutover';
    END IF;
END
$guard$;

DROP TABLE pg_temp.cvm_stack_release_events, pg_temp.cvm_stack_observations, pg_temp.cvm_stack_releases;
@TARGET_SCHEMA@

CREATE FUNCTION pg_temp.legacy_stack_identity(artifact jsonb) RETURNS jsonb
LANGUAGE plpgsql IMMUTABLE STRICT AS $identity$
DECLARE
    payload jsonb;
    source jsonb;
    target jsonb;
    contract text;
    prefix text;
    digest_key text;
BEGIN
    IF (artifact - 'payload' - 'signature') <> '{}'::jsonb
       OR jsonb_typeof(artifact->'payload') IS DISTINCT FROM 'string'
       OR octet_length(convert_to(artifact->>'payload', 'UTF8')) NOT BETWEEN 1 AND 4096
       OR jsonb_typeof(artifact->'signature') IS DISTINCT FROM 'string'
       OR (artifact->>'signature') !~ '^[0-9a-f]{128}$' THEN
        RAISE EXCEPTION 'invalid legacy signed Cube Stack artifact';
    END IF;
    BEGIN
        payload := (artifact->>'payload')::jsonb;
    EXCEPTION WHEN data_exception THEN
        RAISE EXCEPTION 'invalid legacy signed Cube Stack payload';
    END;
    contract := payload->>'contract';
    IF jsonb_typeof(payload) IS DISTINCT FROM 'object'
       OR NOT coalesce(contract IN ('brocade-cvm-stack-fresh-install-v1', 'brocade-cvm-stack-cold-pair-v1'), false)
       OR NOT coalesce(payload->>'architecture' = 'x86_64', false)
       OR NOT coalesce(payload->>'storage_backend' IN ('plain', 'xfs'), false)
       OR jsonb_typeof(payload->'expires_at') IS DISTINCT FROM 'number'
       OR (payload->>'expires_at') !~ '^[1-9][0-9]*$'
       OR jsonb_typeof(payload->'target_bytes') IS DISTINCT FROM 'number'
       OR (payload->>'target_bytes') !~ '^[1-9][0-9]*$'
       OR (payload - ARRAY['contract', 'architecture', 'storage_backend', 'source_sha256', 'target_sha256', 'target_bytes',
                'source_cubelet_sha256', 'source_shim_sha256', 'target_cubelet_sha256', 'target_shim_sha256', 'expires_at']) <> '{}'::jsonb THEN
        RAISE EXCEPTION 'invalid legacy signed Cube Stack payload';
    END IF;
    IF (payload->>'expires_at')::numeric > 18446744073709551615 OR (payload->>'target_bytes')::numeric > 8589934592 THEN
        RAISE EXCEPTION 'invalid legacy signed Cube Stack payload bounds';
    END IF;
    FOREACH prefix IN ARRAY ARRAY['source', 'target'] LOOP
        FOREACH digest_key IN ARRAY ARRAY[prefix || '_sha256', prefix || '_cubelet_sha256', prefix || '_shim_sha256'] LOOP
            IF prefix = 'source' AND contract = 'brocade-cvm-stack-fresh-install-v1' THEN
                IF payload ? digest_key AND payload->digest_key <> 'null'::jsonb THEN
                    RAISE EXCEPTION 'unexpected source identity for legacy Cube Stack installation';
                END IF;
            ELSIF jsonb_typeof(payload->digest_key) IS DISTINCT FROM 'string' OR (payload->>digest_key) !~ '^[0-9a-f]{64}$' THEN
                RAISE EXCEPTION 'invalid legacy Cube Stack bundle identity';
            END IF;
        END LOOP;
    END LOOP;
    IF contract = 'brocade-cvm-stack-cold-pair-v1' THEN
        IF payload->>'source_sha256' = payload->>'target_sha256' THEN
            RAISE EXCEPTION 'identical source and target for legacy Cube Stack upgrade';
        END IF;
        source := jsonb_build_object('bundle_sha256', payload->'source_sha256', 'cubelet_sha256', payload->'source_cubelet_sha256',
            'shim_sha256', payload->'source_shim_sha256', 'storage_backend', payload->'storage_backend');
    END IF;
    target := jsonb_build_object('bundle_sha256', payload->'target_sha256', 'cubelet_sha256', payload->'target_cubelet_sha256',
        'shim_sha256', payload->'target_shim_sha256', 'storage_backend', payload->'storage_backend');
    RETURN jsonb_build_object('pair_id', encode(sha256(convert_to(artifact->>'payload', 'UTF8')), 'hex'), 'source', source, 'target', target);
END
$identity$;

CREATE TEMP TABLE releases_expected ON COMMIT DROP AS
    SELECT r.id, e.created_at, (to_jsonb(r) - 'artifact' - 'expires_at') || jsonb_build_object(
        'artifact', pg_temp.legacy_stack_identity(r.artifact), 'start_deadline_at', r.expires_at, 'created_at', e.created_at) AS record
    FROM public.cvm_stack_releases r LEFT JOIN LATERAL (
        SELECT created_at, status, actor_kind FROM public.cvm_stack_release_events
        WHERE release_id = r.id ORDER BY id LIMIT 1
    ) e ON e.status = 'preparing' AND e.actor_kind = 'admin';
CREATE TEMP TABLE events_before ON COMMIT DROP AS SELECT id, to_jsonb(e) AS record FROM public.cvm_stack_release_events e;
CREATE TEMP TABLE receipts_before ON COMMIT DROP AS SELECT version, to_jsonb(m) AS record FROM public._sqlx_migrations m;

DO $check$
BEGIN
    IF EXISTS (SELECT FROM releases_expected WHERE created_at IS NULL OR NOT isfinite(created_at)) THEN
        RAISE EXCEPTION 'legacy Cube Stack release lacks its initial preparation audit event';
    END IF;
END
$check$;

DROP INDEX public.cvm_stack_releases_open_host;
ALTER TABLE public.cvm_stack_releases
    DROP CONSTRAINT cvm_stack_releases_status_check,
    DROP CONSTRAINT cvm_stack_releases_check,
    ADD COLUMN created_at timestamptz;
ALTER TABLE public.cvm_stack_releases RENAME COLUMN expires_at TO start_deadline_at;
ALTER TABLE public.cvm_stack_releases RENAME CONSTRAINT cvm_stack_releases_expires_at_check TO cvm_stack_releases_start_deadline_at_check;
-- PostgreSQL 18 assigns a name to NOT NULL; recreate it under the new column name.
ALTER TABLE public.cvm_stack_releases ALTER COLUMN start_deadline_at DROP NOT NULL;
UPDATE public.cvm_stack_releases r SET artifact = e.record->'artifact', created_at = e.created_at
FROM releases_expected e WHERE e.id = r.id;
ALTER TABLE public.cvm_stack_releases
    ALTER COLUMN start_deadline_at SET NOT NULL,
    ALTER COLUMN created_at SET DEFAULT now(),
    ALTER COLUMN created_at SET NOT NULL,
    ADD CONSTRAINT cvm_stack_releases_status_check CHECK (status IN (
        'preparing', 'ready', 'applying', 'succeeded', 'failed', 'recovered', 'blocked', 'cancelled'
    )),
    ADD CONSTRAINT cvm_stack_releases_check CHECK (status NOT IN ('ready', 'applying', 'succeeded', 'recovered', 'blocked')
        OR roots_sha256 IS NOT NULL);
CREATE UNIQUE INDEX cvm_stack_releases_open_host ON public.cvm_stack_releases(node_id)
WHERE status IN ('preparing', 'ready', 'applying', 'blocked');

-- Legacy positive observations cannot authorize admission with the new contract.
TRUNCATE public.cvm_stack_observations;

DO $verify$
BEGIN
    IF pg_temp.stack_schema('public'::regnamespace, 'p') IS DISTINCT FROM pg_temp.stack_schema(pg_my_temp_schema(), 't') THEN
        RAISE EXCEPTION 'converted Cube Stack schema differs from pristine target';
    END IF;
    IF EXISTS (SELECT FROM releases_expected x FULL JOIN public.cvm_stack_releases r USING (id)
               WHERE x.record IS DISTINCT FROM to_jsonb(r))
       OR EXISTS (SELECT FROM events_before x FULL JOIN public.cvm_stack_release_events e USING (id)
                  WHERE x.record IS DISTINCT FROM to_jsonb(e))
       OR EXISTS (SELECT FROM public.cvm_stack_observations) THEN
        RAISE EXCEPTION 'converted Cube Stack release, audit or observation verification failed';
    END IF;
END
$verify$;

UPDATE public._sqlx_migrations SET checksum = decode('@TARGET_SHA384@', 'hex') WHERE version = 9;
DO $verify_receipts$
BEGIN
    IF EXISTS (SELECT FROM receipts_before x FULL JOIN public._sqlx_migrations m USING (version)
               WHERE x.record - 'checksum' IS DISTINCT FROM to_jsonb(m) - 'checksum'
                  OR CASE WHEN m.version = 9 THEN m.checksum IS DISTINCT FROM decode('@TARGET_SHA384@', 'hex')
                          ELSE x.record->'checksum' IS DISTINCT FROM to_jsonb(m)->'checksum' END) THEN
        RAISE EXCEPTION 'migration receipt verification failed';
    END IF;
END
$verify_receipts$;
SELECT count(*) AS converted_terminal_releases FROM releases_expected;
COMMIT;
