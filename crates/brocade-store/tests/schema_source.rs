use std::{fs, path::PathBuf};

const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_init.sql");
const VPNGATE_STORE: &str = include_str!("../src/vpngate.rs");

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
fn vpngate_probe_history_stays_a_small_diagnostic_store() {
    assert_eq!(
        INITIAL_SCHEMA
            .matches("CREATE INDEX vpngate_candidate_probe_samples_")
            .count(),
        2,
        "probe history should have only one diagnostic index and one retention index in addition to its primary key",
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
