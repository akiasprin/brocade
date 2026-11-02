-- User/app constant-speed policies, independent of quota existence.
-- Constant speed is independent of quota existence; the quota remains its sole allowance source.
CREATE TABLE user_app_rate_limits (
    tenant_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    app_id TEXT NOT NULL REFERENCES apps(id) ON UPDATE CASCADE ON DELETE CASCADE,
    base_rate_bps BIGINT NOT NULL CHECK (base_rate_bps BETWEEN 1000 AND 1000000000000),
    revision BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,user_id,app_id),
    FOREIGN KEY (tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE OR REPLACE VIEW user_traffic_scopes AS
    SELECT COALESCE(q.tenant_id,r.tenant_id) AS tenant_id,COALESCE(q.user_id,r.user_id) AS user_id,
           'access'::text AS kind,COALESCE(q.app_id,r.app_id) AS scope,
           q.limit_bytes AS quota_bytes,COALESCE(q.exhausted_action,'suspend') AS action,q.rate_bps,
           COALESCE(q.exhausted,FALSE) AS exhausted,
           COALESCE(q.exhausted AND q.exhausted_action='suspend',FALSE) AS suspended,
           COALESCE(q.revision,0)+COALESCE(r.revision,0) AS revision,r.base_rate_bps,
           r.base_rate_bps IS NOT NULL OR (q.limit_bytes IS NOT NULL AND q.exhausted_action='throttle') AS budget_managed
    FROM user_app_quotas q FULL JOIN user_app_rate_limits r USING(tenant_id,user_id,app_id);
-- Quota removal must not reset a live constant-speed reservation clock or its replay receipts.
CREATE OR REPLACE FUNCTION user_traffic_quota_deleted() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM user_traffic_budgets b
    WHERE b.tenant_id=OLD.tenant_id AND b.user_id=OLD.user_id AND b.kind='access' AND b.scope=OLD.app_id
      AND NOT EXISTS(SELECT 1 FROM user_traffic_scopes p WHERE p.tenant_id=b.tenant_id AND p.user_id=b.user_id
                     AND p.scope=b.scope AND p.budget_managed);
    RETURN OLD;
END
$$;
CREATE TRIGGER user_app_base_rate_deleted AFTER DELETE ON user_app_rate_limits
    FOR EACH ROW EXECUTE FUNCTION user_traffic_quota_deleted();
CREATE TRIGGER agent_channel_access_base_rate AFTER INSERT OR DELETE OR UPDATE OF revision ON user_app_rate_limits
    FOR EACH ROW EXECUTE FUNCTION notify_agent_channel_change('user_traffic');
-- END USER APP BASE RATE
