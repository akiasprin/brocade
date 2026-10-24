const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_init.sql");
const FORWARDING_DEFAULTS_COMPAT: &str =
    include_str!("../../../scripts/compat-forwarding-entitlement-defaults.sql");
const CONFIG_XRAY_FORCE_RETIREMENT_COMPAT: &str =
    include_str!("../../../scripts/compat-config-xray-force-retirement.sql");
const VPNGATE_WORKER_SETS_COMPAT: &str =
    include_str!("../../../scripts/compat-vpngate-worker-sets.sql");
const VPNGATE_STORE: &str = include_str!("../src/vpngate.rs");

#[test]
fn data_retention_compatibility_schema_matches_fresh_install() {
    let fresh = INITIAL_SCHEMA
        .split_once("CREATE TABLE data_retention_policies")
        .unwrap()
        .1
        .split_once("CREATE TABLE apps")
        .unwrap()
        .0;
    let compat_source = include_str!("../../../scripts/compat-data-retention.sql");
    let compat = compat_source
        .split_once("CREATE TABLE data_retention_policies")
        .unwrap()
        .1
        .split_once("COMMIT;")
        .unwrap()
        .0;
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
    };
    assert_eq!(normalize(fresh), normalize(compat));
    assert!(compat_source.contains("SET LOCAL ROLE :\"app_role\""));
}

#[test]
fn forwarding_quota_compatibility_schema_matches_fresh_install() {
    let block = |source: &str| {
        source
            .split_once("-- BEGIN FORWARDING QUOTA SCHEMA")
            .unwrap()
            .1
            .split_once("-- END FORWARDING QUOTA SCHEMA")
            .unwrap()
            .0
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    let compat = include_str!("../../../scripts/compat-forwarding-quota.sql");
    assert_eq!(block(INITIAL_SCHEMA), block(compat));
    assert!(compat.contains("SET LOCAL ROLE :\"app_role\""));
    assert!(!compat.contains("_sqlx_migrations"));
}

#[test]
fn node_region_override_compatibility_sql_matches_fresh_schema() {
    let fresh = INITIAL_SCHEMA
        .split_once("CREATE TABLE node_region_overrides")
        .unwrap()
        .1
        .split_once("-- Catalogue measurement")
        .unwrap()
        .0;
    let compat_source = include_str!("../../../scripts/compat-node-region-overrides.sql");
    let compat = compat_source
        .split_once("CREATE TABLE node_region_overrides")
        .unwrap()
        .1
        .split_once("COMMIT;")
        .unwrap()
        .0;
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
    };
    assert_eq!(normalize(fresh), normalize(compat));
    assert!(compat_source.contains("SET LOCAL ROLE :\"app_role\""));
}

#[test]
fn vpngate_worker_sets_are_independent_and_default_off() {
    assert!(INITIAL_SCHEMA.contains("enabled BOOLEAN DEFAULT false NOT NULL"));
    assert!(INITIAL_SCHEMA.contains("CREATE TABLE vpngate_catalog_nodes"));
    assert!(INITIAL_SCHEMA.contains("CREATE TABLE vpngate_intelligence_nodes"));
    assert!(VPNGATE_WORKER_SETS_COMPAT
        .contains("ALTER TABLE vpngate_intelligence_nodes RENAME TO vpngate_catalog_nodes"));
    assert!(VPNGATE_WORKER_SETS_COMPAT
        .contains("INSERT INTO vpngate_intelligence_nodes (node_id, selected_by, selected_at)"));
    assert!(VPNGATE_WORKER_SETS_COMPAT.contains("ALTER COLUMN enabled SET DEFAULT false"));
    assert!(VPNGATE_WORKER_SETS_COMPAT.contains("enabled = false"));
    assert!(VPNGATE_WORKER_SETS_COMPAT.contains("SET LOCAL ROLE :\"app_role\""));
}

#[test]
fn sniffing_failed_compatibility_sql_matches_fresh_schema() {
    let fresh = INITIAL_SCHEMA
        .split_once("-- BEGIN SNIFFING FAILED USAGE SCHEMA")
        .unwrap()
        .1
        .split_once("-- END SNIFFING FAILED USAGE SCHEMA")
        .unwrap()
        .0;
    let compat = include_str!("../../../scripts/compat-sniffing-failed-usage.sql");
    let ddl = compat
        .split_once("CREATE TABLE node_sniffing_failed_state")
        .unwrap()
        .1
        .split_once("COMMIT;")
        .unwrap()
        .0;
    let fresh = fresh
        .split_once("CREATE TABLE node_sniffing_failed_state")
        .unwrap()
        .1;
    let normalize = |v: &str| v.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    assert_eq!(normalize(fresh), normalize(ddl));
    assert!(INITIAL_SCHEMA.contains("\"enabled\":true,\"top_n\":128,\"max_targets_per_user\":1024,\"node_capacity\":4096,\"report_interval_secs\":30"));
    assert!(compat.contains("SET LOCAL ROLE :\"app_role\""));
}

#[test]
fn other_domain_compatibility_sql_matches_fresh_schema() {
    let fresh = INITIAL_SCHEMA
        .split_once("-- BEGIN OTHER DOMAIN USAGE SCHEMA")
        .unwrap()
        .1
        .split_once("-- END OTHER DOMAIN USAGE SCHEMA")
        .unwrap()
        .0;
    let compat = include_str!("../../../scripts/compat-other-domain-usage.sql");
    let ddl = compat
        .split_once("CREATE TABLE node_other_domain_state")
        .unwrap()
        .1
        .split_once("COMMIT;")
        .unwrap()
        .0;
    let fresh = fresh
        .split_once("CREATE TABLE node_other_domain_state")
        .unwrap()
        .1;
    let normalize = |v: &str| v.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    assert_eq!(normalize(fresh), normalize(ddl));
    assert!(INITIAL_SCHEMA.contains("\"enabled\":true,\"top_n\":128,\"max_domains_per_user\":1024,\"node_capacity\":4096,\"report_interval_secs\":30"));
    assert!(compat.contains("SET LOCAL ROLE :\"app_role\""));
}

#[test]
fn site_usage_compatibility_sql_matches_fresh_schema() {
    let fresh = INITIAL_SCHEMA
        .split_once("-- BEGIN USER SITE USAGE SCHEMA")
        .unwrap()
        .1
        .split_once("-- END USER SITE USAGE SCHEMA")
        .unwrap()
        .0;
    assert!(INITIAL_SCHEMA.contains("site_usage_enabled BOOLEAN NOT NULL DEFAULT TRUE"));
    let compat = include_str!("../../../scripts/compat-user-site-usage.sql")
        .split_once("ALTER TABLE control_state ADD COLUMN site_usage_enabled BOOLEAN NOT NULL DEFAULT FALSE;")
        .unwrap()
        .1
        .split_once("COMMIT;")
        .unwrap()
        .0;
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    assert_eq!(normalize(fresh), normalize(compat));
}

#[test]
fn new_ingresses_default_to_strict_reality_fallback_limits() {
    assert!(INITIAL_SCHEMA
        .contains("reality_fallback_limits JSONB DEFAULT '{\"mode\":\"strict\"}'::jsonb NOT NULL"));
}

#[test]
fn fresh_install_log_limits_match_the_runtime_defaults() {
    assert!(INITIAL_SCHEMA.contains("agent_log_max_mib INTEGER DEFAULT 20 NOT NULL"));
    assert!(INITIAL_SCHEMA.contains("xray_log_max_mib INTEGER DEFAULT 20 NOT NULL"));
    assert!(INITIAL_SCHEMA.contains("phantun_log_max_mib INTEGER DEFAULT 10 NOT NULL"));
    assert!(INITIAL_SCHEMA
        .contains("control_state_agent_log_max_mib_range CHECK (((agent_log_max_mib >= 10)"));
    assert!(INITIAL_SCHEMA
        .contains("control_state_xray_log_max_mib_range CHECK (((xray_log_max_mib >= 10)"));
    assert!(INITIAL_SCHEMA
        .contains("control_state_phantun_log_max_mib_range CHECK (((phantun_log_max_mib >= 10)"));
}

#[test]
fn fresh_install_host_network_tuning_matches_the_runtime_defaults() {
    assert!(INITIAL_SCHEMA.contains("nic_gro_flush_timeout_ns INTEGER DEFAULT 20000 NOT NULL"));
    assert!(INITIAL_SCHEMA.contains("nic_napi_defer_hard_irqs INTEGER DEFAULT 2 NOT NULL"));
    assert!(INITIAL_SCHEMA.contains(
        "control_state_nic_gro_flush_timeout_ns_range CHECK (((nic_gro_flush_timeout_ns >= 0)"
    ));
    assert!(INITIAL_SCHEMA.contains(
        "control_state_nic_napi_defer_hard_irqs_range CHECK (((nic_napi_defer_hard_irqs >= 0)"
    ));
}

#[test]
fn traffic_accounting_modes_have_fresh_and_existing_schema_paths() {
    let compat = include_str!("../../../scripts/compat-traffic-accounting-modes.sql");
    for expected in [
        "traffic_accounting_mode TEXT DEFAULT 'single' NOT NULL",
        "accounting_mode TEXT DEFAULT 'bidirectional_sum' NOT NULL",
        "accounted_rx_bytes NUMERIC(20,0)",
        "accounted_tx_bytes NUMERIC(20,0)",
        "accounted_rx_bytes IS NOT NULL",
        "accounted_tx_bytes IS NOT NULL",
    ] {
        assert!(
            INITIAL_SCHEMA.contains(expected),
            "fresh schema missing {expected}"
        );
        assert!(
            compat.contains(expected),
            "compatibility SQL missing {expected}"
        );
    }
    assert!(compat.contains("SET LOCAL ROLE :\"app_role\""));
    assert!(!compat.contains("_sqlx_migrations"));
}

#[test]
fn fresh_install_mtproxy_port_base_matches_the_runtime_default() {
    assert!(INITIAL_SCHEMA.contains("port_mtproto_base INTEGER DEFAULT 28800 NOT NULL"));
}

#[test]
fn fresh_install_includes_xray_handoff_policy_and_transient_process_state() {
    for expected in [
        "xray_upgrade JSONB",
        "xray_strategy TEXT",
        "xray_drain_timeout_seconds INTEGER",
        "reported_forced_retirement BOOLEAN",
        "binary_releases_xray_execution_shape",
        "node_agent_state_xray_upgrade_object",
    ] {
        assert!(INITIAL_SCHEMA.contains(expected), "missing {expected}");
    }
}

#[test]
fn fresh_and_existing_databases_include_exact_config_force_authorizations() {
    for expected in [
        "CREATE TABLE config_xray_force_retirements",
        "PRIMARY KEY (deployment_id, node_id, claim_generation)",
        "FOREIGN KEY (deployment_id, node_id)",
        "CREATE UNIQUE INDEX config_xray_force_retirements_one_active_per_node",
        "WHERE settled_at IS NULL",
    ] {
        assert!(
            INITIAL_SCHEMA.contains(expected),
            "fresh schema missing {expected}"
        );
        assert!(
            CONFIG_XRAY_FORCE_RETIREMENT_COMPAT.contains(expected),
            "compatibility SQL missing {expected}"
        );
    }
}

#[test]
fn fresh_and_existing_forwarding_grants_share_the_current_defaults() {
    for expected in [
        "listen_port_start INTEGER DEFAULT 30000 NOT NULL",
        "listen_port_end INTEGER DEFAULT 59999 NOT NULL",
        "max_rules INTEGER DEFAULT 100 NOT NULL",
        "allow_private_targets BOOLEAN DEFAULT true NOT NULL",
    ] {
        assert!(INITIAL_SCHEMA.contains(expected), "missing {expected}");
    }
    for expected in [
        "ALTER COLUMN listen_port_start SET DEFAULT 30000",
        "ALTER COLUMN listen_port_end SET DEFAULT 59999",
        "ALTER COLUMN max_rules SET DEFAULT 100",
        "ALTER COLUMN allow_private_targets SET DEFAULT true",
    ] {
        assert!(
            FORWARDING_DEFAULTS_COMPAT.contains(expected),
            "missing {expected}"
        );
    }
}

#[test]
fn certificate_scan_compatibility_block_is_extractable() {
    assert!(INITIAL_SCHEMA.contains("renew_before_days INTEGER DEFAULT 60 NOT NULL"));
    assert_eq!(
        INITIAL_SCHEMA
            .matches("-- BEGIN CERTIFICATE SCAN SCHEMA")
            .count(),
        1
    );
    assert_eq!(
        INITIAL_SCHEMA
            .matches("-- END CERTIFICATE SCAN SCHEMA")
            .count(),
        1
    );
    assert!(INITIAL_SCHEMA.contains("CREATE TABLE certificate_scan_runs"));
}

#[test]
fn initial_schema_is_a_fresh_install_definition() {
    let upper = INITIAL_SCHEMA.to_ascii_uppercase();
    for forbidden in [
        "ADD COLUMN",
        "ALTER COLUMN",
        "DROP COLUMN",
        "RENAME COLUMN",
        "IF EXISTS",
        "IF NOT EXISTS",
        "CREATE OR REPLACE",
    ] {
        assert!(
            !upper.contains(forbidden),
            "0001_init.sql contains incremental-migration construct {forbidden}"
        );
    }
}

#[test]
fn one_direct_login_credential_stays_on_the_user_row() {
    assert!(INITIAL_SCHEMA.contains("direct_login_token_hash TEXT"));
    assert!(INITIAL_SCHEMA.contains("users_direct_login_token_hash_key UNIQUE"));
    assert!(
        !INITIAL_SCHEMA.contains("CREATE TABLE user_direct_logins"),
        "a one-to-one credential does not need a second table"
    );
}

#[test]
fn vpngate_probe_history_stays_a_small_diagnostic_store() {
    assert_eq!(
        INITIAL_SCHEMA
            .matches("CREATE INDEX vpngate_candidate_probe_samples_")
            .count(),
        1,
        "probe history should keep only the BRIN retention index in addition to its primary key",
    );
    assert!(INITIAL_SCHEMA.contains("PRIMARY KEY (node_id, server_id, profile_sha256, probed_at)"));

    assert!(!VPNGATE_STORE.contains("JOIN vpngate_candidate_probe_samples"));
    assert!(VPNGATE_STORE.contains("INSERT INTO vpngate_candidate_probe_samples"));
    assert!(!VPNGATE_STORE.contains("FROM vpngate_candidate_probe_samples"));
    assert!(!VPNGATE_STORE.contains("FROM vpngate_probe_samples"));
}

#[test]
fn intelligence_due_index_covers_expired_leases_without_indexing_every_sighting() {
    assert!(INITIAL_SCHEMA.contains(
        "CREATE INDEX vpngate_exit_reputations_due\n    ON vpngate_exit_reputations (next_check_at, exit_ip);"
    ));
    assert!(VPNGATE_STORE.contains("AND (lease_until IS NULL OR lease_until <= now())"));
    assert!(VPNGATE_STORE.contains("ORDER BY next_check_at, exit_ip"));
}

#[test]
fn user_machine_usage_compatibility_matches_fresh_schema() {
    let block = |source: &str| {
        source
            .split_once("-- BEGIN USER MACHINE USAGE SCHEMA")
            .unwrap()
            .1
            .split_once("-- END USER MACHINE USAGE SCHEMA")
            .unwrap()
            .0
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    assert_eq!(
        block(INITIAL_SCHEMA),
        block(include_str!(
            "../../../scripts/compat-user-machine-usage.sql"
        ))
    );
}

#[test]
fn user_ingress_usage_compatibility_matches_fresh_schema() {
    let block = |source: &str| {
        source
            .split_once("-- BEGIN USER INGRESS USAGE SCHEMA")
            .unwrap()
            .1
            .split_once("-- END USER INGRESS USAGE SCHEMA")
            .unwrap()
            .0
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    let compat = include_str!("../../../scripts/compat-user-ingress-usage.sql");
    assert_eq!(block(INITIAL_SCHEMA), block(compat));
    // The backfill runs once, as the application owner, from retained detail only.
    assert!(compat.contains("SET LOCAL ROLE :\"app_role\""));
    assert!(compat.contains("FROM usage_samples s LEFT JOIN ingresses i ON i.id=s.ingress_id"));
    assert!(!compat.contains("ON CONFLICT"));
}

#[test]
fn user_online_session_compatibility_preserves_legacy_unknowns() {
    let compat = include_str!("../../../scripts/compat-user-online-session.sql");
    for fragment in [
        "online_since_at TIMESTAMPTZ",
        "session_uplink_bytes BIGINT",
        "session_downlink_bytes BIGINT",
        "user_online_sources_session_shape",
        "online_since_at IS NULL",
        "session_uplink_bytes IS NOT NULL",
        "session_downlink_bytes IS NOT NULL",
        "online_since_at <= last_observed_at",
        "session_uplink_bytes >= 0",
        "session_downlink_bytes >= 0",
    ] {
        assert!(INITIAL_SCHEMA.contains(fragment));
        assert!(compat.contains(fragment));
    }
    assert!(compat.contains("SET LOCAL ROLE :\"app_role\""));
    assert!(compat.contains("VALIDATE CONSTRAINT user_online_sources_session_shape"));
}
