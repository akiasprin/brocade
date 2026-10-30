#[path = "../build/migrations.rs"]
mod migrations;

use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("brocade-migrations-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, filename: &str) {
        fs::write(self.0.join(filename), "SELECT 1;\n").unwrap();
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn repository_keeps_consolidated_baseline_and_appends_unlock_schedule() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let sources = migrations::sources(&directory).unwrap();
    assert_eq!(
        sources
            .iter()
            .map(|source| source.version)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let notifications = fs::read_to_string(&sources[2].path).unwrap();
    assert!(notifications.contains("pg_notify"));
    assert!(notifications.contains("AFTER UPDATE OF status, attempt ON binary_release_targets"));
    let schedule = fs::read_to_string(&sources[2].path).unwrap();
    assert!(schedule.contains("CREATE TABLE unlock_probe_jobs"));
    assert!(schedule.contains("CREATE TABLE unlock_probe_discovery"));
    assert!(schedule.contains("ADD COLUMN address_family"));
    assert!(
        !schedule.contains("SET address_family"),
        "legacy automatic results must not be assigned a family"
    );
    assert!(schedule.contains("unlock_refresh_stale BOOLEAN NOT NULL DEFAULT TRUE"));
    assert!(
        !schedule.contains("SET address_family"),
        "settings migration must preserve current observations"
    );
    assert!(schedule.contains("ADD COLUMN readiness_failures"));
    let second = fs::read_to_string(&sources[1].path).unwrap();
    assert!(second.contains("ADD COLUMN target_strategy TEXT"));
    assert!(second.contains(
        "ALTER TABLE user_traffic_policies RENAME TO retired_forwarding_traffic_policies"
    ));
}

#[test]
fn discovers_all_migrations_in_numeric_order() {
    let directory = TestDirectory::new();
    for filename in [
        "10_last_step.sql",
        "0002_add_rules.sql",
        "0001_init.sql",
        "4_backfill_rules.sql",
    ] {
        directory.write(filename);
    }
    let sources = migrations::sources(&directory.0).unwrap();
    assert_eq!(
        sources
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>(),
        [1, 2, 4, 10]
    );
    assert_eq!(sources[1].description, "add rules");
    assert!(PathBuf::from(&sources[1].path).is_absolute());
    assert_eq!(fs::read_to_string(&sources[1].path).unwrap(), "SELECT 1;\n");
}

#[test]
fn rejects_duplicate_numeric_versions() {
    let directory = TestDirectory::new();
    for filename in ["0001_init.sql", "0002_one.sql", "2_another.sql"] {
        directory.write(filename);
    }
    let error = migrations::sources(&directory.0).err().unwrap();
    assert!(error.to_string().contains("duplicate migration version 2"));
}

#[test]
fn rejects_invalid_files_instead_of_silently_omitting_migrations() {
    for filename in [
        "notes.txt",
        "missing_version.sql",
        "0000_zero.sql",
        "-1_negative.sql",
        "+2_signed.sql",
        "9223372036854775808_overflow.sql",
        "0002_.sql",
        "0002_change.up.sql",
        "0002_change.down.sql",
    ] {
        let directory = TestDirectory::new();
        directory.write("0001_init.sql");
        directory.write(filename);
        assert!(
            migrations::sources(&directory.0).is_err(),
            "accepted invalid migration {filename}"
        );
    }
}

#[test]
fn requires_the_initial_migration_without_limiting_later_versions() {
    let directory = TestDirectory::new();
    assert!(migrations::sources(&directory.0).is_err());
    directory.write("0002_next.sql");
    assert!(migrations::sources(&directory.0).is_err());
    directory.write("0001_init.sql");
    assert_eq!(migrations::sources(&directory.0).unwrap().len(), 2);
}

#[test]
fn rejects_directories_inside_the_migration_chain() {
    let directory = TestDirectory::new();
    directory.write("0001_init.sql");
    fs::create_dir(directory.0.join("0002_nested.sql")).unwrap();
    assert!(migrations::sources(&directory.0).is_err());
}
