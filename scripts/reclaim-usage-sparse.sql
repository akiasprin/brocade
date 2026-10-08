-- Run only after compat-usage-sparse.sql committed successfully and before enabling writers.
-- Failure here does NOT make the old Console compatible again. Keep the new schema and recover
-- forward; no implicit database restore or old-binary restart.
\set ON_ERROR_STOP on
SET ROLE :"app_role";
SET lock_timeout = '5s';
SET statement_timeout = '15min';
BEGIN;
CLUSTER usage_samples USING usage_samples_pkey;
ALTER TABLE usage_samples SET WITHOUT CLUSTER;
ANALYZE usage_samples;
COMMIT;
