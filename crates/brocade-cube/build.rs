use prost::Message;
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let cube = root.join("../../components/cube/pkgs/proto");
    let containerd = root.join("proto");
    let files = [
        cube.join("services/cubebox/v1/cubebox.proto"),
        containerd.join("github.com/containerd/containerd/api/services/tasks/v1/tasks.proto"),
    ];
    let descriptors = protox::compile(files, [&cube, &containerd])?;
    for file in &descriptors.file {
        if let Some(name) = &file.name {
            for include in [&cube, &containerd] {
                let path = include.join(name);
                if path.is_file() {
                    println!("cargo:rerun-if-changed={}", path.display());
                }
            }
        }
    }
    fs::write(
        PathBuf::from(env::var("OUT_DIR")?).join("cube.bin"),
        descriptors.encode_to_vec(),
    )?;
    tonic_prost_build::configure()
        .include_file("generated.rs")
        .generate_default_stubs(true)
        .compile_fds(descriptors)?;
    Ok(())
}
