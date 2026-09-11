use std::{fs, path::PathBuf};

const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_init.sql");

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
