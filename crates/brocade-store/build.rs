fn main() {
    // `sqlx::migrate!` reads this directory from a proc macro. Cargo cannot discover that file
    // dependency on its own, so editing an existing migration could otherwise leave a cached
    // checksum embedded in brocade-store and every Console binary that links it.
    println!("cargo:rerun-if-changed=migrations");
}
