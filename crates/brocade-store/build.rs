#[path = "build/migrations.rs"]
mod migrations;

fn main() -> std::io::Result<()> {
    // Track directory changes as well as contents so appended migrations enter the next binary.
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-changed=build/migrations.rs");
    let directory = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .expect("Cargo sets CARGO_MANIFEST_DIR for build scripts"),
    )
    .join("migrations");
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR for build scripts"),
    )
    .join("migrations.rs");
    let mut source = String::from("&[\n");
    for migration in migrations::sources(&directory)? {
        source.push_str(&format!(
            "    ({}, {:?}, include_str!({:?})),\n",
            migration.version, migration.description, migration.path
        ));
    }
    source.push_str("]\n");
    std::fs::write(output, source)
}
