use std::{fs, path::PathBuf};

const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_init.sql");
const VPNGATE_STORE: &str = include_str!("../src/vpngate.rs");

#[test]
fn new_ingresses_default_to_strict_reality_fallback_limits() {
    assert!(INITIAL_SCHEMA
        .contains("reality_fallback_limits JSONB DEFAULT '{\"mode\":\"strict\"}'::jsonb NOT NULL"));
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
