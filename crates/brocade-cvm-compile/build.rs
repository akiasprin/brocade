use prost::Message;
use std::{env, fs, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("../../components/cube/pkgs/proto");
    let descriptors = protox::compile([root.join("services/cubebox/v1/cubebox.proto")], [&root])?;
    for file in &descriptors.file {
        if let Some(name) = &file.name {
            println!("cargo:rerun-if-changed={}", root.join(name).display());
        }
    }
    fs::write(
        PathBuf::from(env::var("OUT_DIR")?).join("cube.bin"),
        descriptors.encode_to_vec(),
    )?;
    Ok(())
}
