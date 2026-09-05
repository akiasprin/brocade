-- Self-signed groups use one exact, synthetic SNI rather than the public-CA
-- `<random-label>.<operator-domain>` wildcard pair.  `.test` is reserved for
-- private testing, so automatically generated names cannot unexpectedly become
-- somebody else's real Internet identity.
ALTER TABLE cert_labels
    ADD COLUMN IF NOT EXISTS certificate_name TEXT,
    ADD COLUMN IF NOT EXISTS is_default BOOLEAN DEFAULT FALSE NOT NULL;

ALTER TABLE cert_labels
    DROP CONSTRAINT IF EXISTS cert_labels_certificate_name_shape;
ALTER TABLE cert_labels
    ADD CONSTRAINT cert_labels_certificate_name_shape CHECK (
        certificate_name IS NULL OR
        certificate_name ~ '^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$'
    );

CREATE UNIQUE INDEX IF NOT EXISTS cert_labels_certificate_name_key
    ON cert_labels (certificate_name)
    WHERE certificate_name IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS cert_labels_one_default
    ON cert_labels (is_default)
    WHERE is_default;

-- Bootstrap rows are distinguishable from operator-created spares in the UI.
ALTER TABLE certificates
    DROP CONSTRAINT IF EXISTS certificates_origin_known;
ALTER TABLE certificates
    ADD CONSTRAINT certificates_origin_known
        CHECK (origin IN ('renewal', 'spare', 'bootstrap'));

-- Serialize every insertion into one self-signed group and enforce the limit below the API layer.
-- This also covers the renewal scanner racing a manual click, or a future writer which forgets
-- the application-side preflight.
CREATE OR REPLACE FUNCTION brocade_enforce_self_signed_pool_limit() RETURNS TRIGGER
    LANGUAGE plpgsql
    AS $$
DECLARE
    self_signed BOOLEAN;
    held BIGINT;
BEGIN
    SELECT d.acme_directory = 'self-signed'
      INTO self_signed
      FROM cert_labels l
      JOIN cert_domains d ON d.id = l.domain_id
     WHERE l.id = NEW.label_id
     FOR UPDATE OF l;
    IF self_signed THEN
        SELECT count(*) INTO held FROM certificates WHERE label_id = NEW.label_id;
        IF held >= 10 THEN
            RAISE EXCEPTION 'self-signed certificate pool may contain at most 10 rows'
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

DROP TRIGGER IF EXISTS certificates_enforce_self_signed_pool_limit ON certificates;
CREATE TRIGGER certificates_enforce_self_signed_pool_limit
    BEFORE INSERT ON certificates
    FOR EACH ROW EXECUTE FUNCTION brocade_enforce_self_signed_pool_limit();
