use std::{fs, path::PathBuf};

const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_init.sql");
const VPNGATE_STORE: &str = include_str!("../src/vpngate.rs");

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
fn schema_source_is_a_single_fresh_install_definition() {
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut files = fs::read_dir(migrations)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files, ["0001_init.sql"]);

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
    assert_eq!(
        VPNGATE_STORE
            .matches("vpngate_candidate_probe_samples")
            .count(),
        3,
        "raw candidate history should appear only in its insert and bounded retention delete",
    );
}

#[test]
fn intelligence_due_index_covers_expired_leases_without_indexing_every_sighting() {
    assert!(INITIAL_SCHEMA.contains(
        "CREATE INDEX vpngate_exit_reputations_due\n    ON vpngate_exit_reputations (next_check_at, exit_ip);"
    ));
    assert!(VPNGATE_STORE.contains("AND (lease_until IS NULL OR lease_until <= now())"));
    assert!(VPNGATE_STORE.contains("ORDER BY next_check_at, exit_ip"));
}
