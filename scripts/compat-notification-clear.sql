-- Additive compatibility for existing Console databases. Does not clear any inbox or history.
-- Verify this definition against 0001 and the previous schema before updating SQLx checksum.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
ALTER TABLE admin_operators
    ADD COLUMN notification_cleared_through_event_id BIGINT DEFAULT 0 NOT NULL;
ALTER TABLE admin_operators
    ADD CONSTRAINT admin_operators_notification_clear_cursor_valid
    CHECK (notification_cleared_through_event_id >= 0
        AND notification_cleared_through_event_id <= notification_last_seen_event_id);
COMMIT;
