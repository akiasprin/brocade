//! Xray's identity is its bytes, not the enclosing Console commit. Verify the actual Go inputs
//! and recipe before accepting either a local build or a prebuilt artifact.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GO_VERSION: &str = include_str!("../../../components/xray-core/.go-version");
const MAX_RECORDS: usize = 128;
const MAX_JSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    go_version: String,
    arch: String,
    version: String,
    environment: BTreeMap<String, String>,
    flags: Vec<String>,
}

impl Recipe {
    pub fn new(arch: &str, version: &str, build_id: &str) -> Result<Self> {
        let go_arch = match arch {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            _ => return Err(format!("unsupported Xray architecture: {arch}").into()),
        };
        Ok(Self {
            go_version: format!("go{}", GO_VERSION.trim()),
            arch: arch.to_owned(),
            version: version.to_owned(),
            environment: [
                ("CGO_ENABLED", "0"),
                ("GOOS", "linux"),
                ("GOARCH", go_arch),
                ("GOTOOLCHAIN", "local"),
                ("GOENV", "off"),
                ("GOFLAGS", ""),
                ("GOWORK", "off"),
                ("GOEXPERIMENT", ""),
                ("GOAMD64", "v1"),
                ("GOARM64", "v8.0"),
                ("GOFIPS140", "off"),
                ("GO111MODULE", "on"),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
            flags: [
                "-mod=readonly".to_owned(),
                "-trimpath".to_owned(),
                "-buildvcs=false".to_owned(),
                "-gcflags=all=-l=4".to_owned(),
                "-pgo=off".to_owned(),
                format!(
                    "-ldflags=-X github.com/xtls/xray-core/core.build={build_id} -s -w -buildid="
                ),
            ]
            .into(),
        })
    }

    fn command(&self, go: &Path, source: &Path) -> Command {
        let mut command = Command::new(go);
        command
            .current_dir(source)
            .envs(&self.environment)
            .env_remove("GOROOT")
            .env_remove("GODEBUG")
            .env_remove("GOCACHEPROG")
            .env_remove("GO_EXTLINK_ENABLED");
        command
    }

    pub fn check_toolchain(&self, go: &Path, source: &Path) -> Result<()> {
        let output = run(self.command(go, source).args(["env", "GOVERSION"]))?;
        let actual = std::str::from_utf8(&output)?.trim();
        if actual != self.go_version {
            return Err(format!(
                "Xray requires exactly {}, found {actual}; install the pinned compiler or set BROCADE_GO",
                self.go_version
            )
            .into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inputs {
    format: u32,
    recipe: Recipe,
    source_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    inputs: Inputs,
    pub binary_sha256: String,
    binary_size: u64,
}

/// This deliberately excludes test-only/unselected files but includes untracked production Go
/// files, go:embed data, assembly, dependencies and the selected standard library. Logical package
/// paths, not checkout/cache paths, identify files so identical builds on two hosts agree.
pub fn inputs(go: &Path, source: &Path, recipe: &Recipe) -> Result<Inputs> {
    let output = run(recipe.command(go, source).args([
        "list",
        "-mod=readonly",
        // Match the build recipe: dependency discovery must not depend on checkout metadata.
        "-buildvcs=false",
        "-deps",
        "-json",
        "./main",
    ]))?;
    let mut files = BTreeMap::new();
    files.insert("module/go.mod".to_owned(), source.join("go.mod"));
    files.insert("module/go.sum".to_owned(), source.join("go.sum"));
    for package in serde_json::Deserializer::from_slice(&output).into_iter::<serde_json::Value>() {
        let package = package?;
        let Some(dir) = package["Dir"].as_str() else {
            continue; // synthetic packages such as unsafe have no source directory
        };
        let import = package["ImportPath"]
            .as_str()
            .ok_or("go list package is missing ImportPath")?;
        for field in [
            "GoFiles",
            "CgoFiles",
            "CFiles",
            "CXXFiles",
            "MFiles",
            "HFiles",
            "FFiles",
            "SFiles",
            "SwigFiles",
            "SwigCXXFiles",
            "SysoFiles",
            "EmbedFiles",
        ] {
            if let Some(names) = package[field].as_array() {
                for name in names {
                    let name = name.as_str().ok_or("go list filename is not a string")?;
                    if Path::new(name)
                        .components()
                        .any(|part| !matches!(part, Component::Normal(_)))
                    {
                        return Err(format!("unsafe Go source filename: {name}").into());
                    }
                    files.insert(
                        format!("package/{import}/{name}"),
                        Path::new(dir).join(name),
                    );
                }
            }
        }
    }
    Ok(Inputs {
        format: 1,
        recipe: recipe.clone(),
        source_sha256: fingerprint(&files)?,
    })
}

fn fingerprint(files: &BTreeMap<String, PathBuf>) -> Result<String> {
    let mut hash = Sha256::new();
    for (name, path) in files {
        let bytes = read_bounded(path, MAX_JSON_BYTES)?;
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".build.json");
    PathBuf::from(name)
}

fn manifest(inputs: &Inputs, bytes: &[u8]) -> Result<Manifest> {
    // Both supported targets are little-endian ELF64. Catch swapped/incorrect artifacts before
    // serving them to nodes, independently of what a sidecar claims.
    let machine: u16 = match inputs.recipe.arch.as_str() {
        "x86_64" => 62,
        "aarch64" => 183,
        _ => return Err("unsupported Xray architecture".into()),
    };
    if bytes.len() < 64
        || &bytes[..6] != b"\x7fELF\x02\x01"
        || bytes[18..20] != machine.to_le_bytes()
    {
        return Err(format!("Xray binary is not a linux/{} ELF64", inputs.recipe.arch).into());
    }
    Ok(Manifest {
        inputs: inputs.clone(),
        binary_sha256: format!("{:x}", Sha256::digest(bytes)),
        binary_size: bytes.len() as u64,
    })
}

fn read_override(path: &Path, expected: &Inputs) -> Result<(Vec<u8>, Manifest)> {
    let record_path = sidecar(path);
    let record: Manifest = serde_json::from_slice(
        &read_bounded(&record_path, MAX_JSON_BYTES).map_err(|error| {
            format!(
                "Xray prebuilt requires its build record {}: {error}; rebuild from current source (unset BROCADE_XRAY_BIN_*)",
                record_path.display()
            )
        })?,
    )?;
    if &record.inputs != expected {
        return Err("Xray prebuilt source/recipe differs from the current build; remove the override and rebuild".into());
    }
    let bytes = read_bounded(path, MAX_BINARY_BYTES)?;
    let actual = manifest(expected, &bytes)?;
    if actual != record {
        return Err("Xray prebuilt digest/size does not match its build record".into());
    }
    Ok((bytes, actual))
}

fn compile(
    go: &Path,
    source: &Path,
    recipe: &Recipe,
    output: &Path,
    fresh: bool,
) -> Result<Vec<u8>> {
    let mut command = recipe.command(go, source);
    command
        .arg("build")
        .args(&recipe.flags)
        .arg("-o")
        .arg(output)
        .arg("./main");
    // A first-seen input set is checked with a genuinely independent compiler cache, not merely a
    // second copy of the first build's cached result. Drop the temporary cache after the check.
    let cache = fresh.then(tempfile::tempdir).transpose()?;
    if let Some(cache) = &cache {
        command.env("GOCACHE", cache.path());
    }
    run(&mut command)?;
    read_bounded(output, MAX_BINARY_BYTES)
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    records: Vec<Manifest>,
}

impl Ledger {
    fn check(&self, candidate: &Manifest) -> Result<bool> {
        if let Some(known) = self
            .records
            .iter()
            .find(|item| item.inputs == candidate.inputs)
        {
            if known != candidate {
                return Err(format!(
                    "Xray reproducibility violation: identical inputs produced {} instead of {}; refusing to replace the verified baseline",
                    candidate.binary_sha256, known.binary_sha256
                ).into());
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn remember(&mut self, candidate: &Manifest) -> Result<()> {
        if !self.check(candidate)? {
            self.records.push(candidate.clone());
            if self.records.len() > MAX_RECORDS {
                self.records.remove(0);
            }
        }
        Ok(())
    }
}

/// Keep evidence outside Cargo OUT_DIR so frontend/Rust profile changes cannot reset it. The
/// ledger is local build evidence, not a signed supply-chain attestation or a production release.
fn ledger<T>(directory: &Path, action: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T> {
    fs::create_dir_all(directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("lock"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(format!("cannot lock Xray build ledger: {error}").into()),
        }
    }
    let path = directory.join("records.json");
    let mut records: Ledger = match read_bounded(&path, MAX_JSON_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ledger::default()
        }
        Err(error) => return Err(error),
    };
    if records.records.len() > MAX_RECORDS {
        return Err("Xray ledger exceeds record limit".into());
    }
    let result = action(&mut records)?;
    atomic_write(&path, &serde_json::to_vec_pretty(&records)?)?;
    Ok(result)
}

pub fn prepare(
    go: &Path,
    source: &Path,
    recipe: &Recipe,
    out_dir: &Path,
    ledger_dir: &Path,
    prebuilt: Option<&Path>,
) -> Result<Manifest> {
    recipe.check_toolchain(go, source)?;
    let before = inputs(go, source, recipe)?;
    let scratch = tempfile::tempdir_in(out_dir)?;
    let (bytes, record) = if let Some(path) = prebuilt {
        read_override(path, &before)?
    } else {
        let bytes = compile(go, source, recipe, &scratch.path().join("xray"), false)?;
        let record = manifest(&before, &bytes)?;
        (bytes, record)
    };
    let known = ledger(ledger_dir, |ledger| ledger.check(&record))?;
    if !known {
        println!(
            "cargo:warning=Xray {}: verifying new build inputs with an independent Go cache",
            recipe.arch
        );
        let rebuilt = compile(go, source, recipe, &scratch.path().join("xray-check"), true)?;
        if manifest(&before, &rebuilt)? != record {
            return Err(
                "Xray independent rebuild disagrees with the candidate; refusing unverified bytes"
                    .into(),
            );
        }
    }
    if before != inputs(go, source, recipe)? {
        return Err("Xray inputs changed during the build; retry with a stable source tree".into());
    }
    // Recheck under the lock: another process may have registered these inputs while we compiled.
    ledger(ledger_dir, |ledger| ledger.remember(&record))?;
    let output = out_dir.join(format!("xray-{}", recipe.arch));
    atomic_write(&output, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&output, fs::Permissions::from_mode(0o755))?;
    }
    atomic_write(&sidecar(&output), &serde_json::to_vec_pretty(&record)?)?;
    println!(
        "cargo:warning=Xray {} verified: source={} binary={} record={}",
        recipe.arch,
        before.source_sha256,
        record.binary_sha256,
        sidecar(&output).display()
    );
    Ok(record)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} exceeds {limit} bytes", path.display()).into());
    }
    Ok(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("artifact has no parent directory")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn run(command: &mut Command) -> Result<Vec<u8>> {
    // Spool output to files while waiting; pipes can deadlock when go list emits a large graph.
    let stdout = tempfile::NamedTempFile::new()?;
    let stderr = tempfile::NamedTempFile::new()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.reopen()?)
        .stderr(stderr.reopen()?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let error = read_bounded(stderr.path(), MAX_JSON_BYTES)?;
                return Err(format!(
                    "Xray Go command failed ({status}): {}",
                    String::from_utf8_lossy(&error)
                )
                .into());
            }
            return read_bounded(stdout.path(), MAX_JSON_BYTES);
        }
        if stdout.as_file().metadata()?.len() > MAX_JSON_BYTES
            || stderr.as_file().metadata()?.len() > MAX_JSON_BYTES
        {
            child.kill()?;
            child.wait()?;
            return Err("Xray Go command exceeded its output limit".into());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err("Xray Go command exceeded 15 minutes".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe() -> Recipe {
        Recipe::new("x86_64", "v26.4.25", "b4f0898").unwrap()
    }

    fn test_inputs() -> Inputs {
        Inputs {
            format: 1,
            recipe: recipe(),
            source_sha256: "test-source".into(),
        }
    }

    fn elf() -> Vec<u8> {
        let mut bytes = vec![0; 64];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
        bytes
    }

    #[test]
    fn recipe_neutralizes_ambient_build_options() {
        let recipe = recipe();
        let command = recipe.command(Path::new("go"), Path::new("."));
        let vars: BTreeMap<_, _> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_str().unwrap(),
                    value.and_then(|value| value.to_str()),
                )
            })
            .collect();
        for (key, value) in &recipe.environment {
            assert_eq!(vars[key.as_str()], Some(value.as_str()));
        }
        for key in ["GOROOT", "GODEBUG", "GOCACHEPROG", "GO_EXTLINK_ENABLED"] {
            assert_eq!(vars[key], None);
        }
        assert!(recipe.flags.contains(&"-buildvcs=false".to_owned()));
        assert!(recipe.flags.contains(&"-pgo=off".to_owned()));
        assert!(Recipe::new("unsupported", "v1", "build").is_err());
    }

    #[test]
    fn fingerprint_is_root_and_insertion_order_independent_but_content_sensitive() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for directory in [a.path(), b.path()] {
            fs::write(directory.join("main.go"), b"code").unwrap();
            fs::write(directory.join("view.html"), b"embed").unwrap();
        }
        let first = BTreeMap::from([
            ("main".into(), a.path().join("main.go")),
            ("view".into(), a.path().join("view.html")),
        ]);
        let second = BTreeMap::from([
            ("view".into(), b.path().join("view.html")),
            ("main".into(), b.path().join("main.go")),
        ]);
        assert_eq!(fingerprint(&first).unwrap(), fingerprint(&second).unwrap());
        fs::write(b.path().join("view.html"), b"changed embed").unwrap();
        assert_ne!(fingerprint(&first).unwrap(), fingerprint(&second).unwrap());
    }

    #[test]
    fn prebuilt_requires_matching_sidecar_source_recipe_and_exact_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("xray");
        let inputs = test_inputs();
        let bytes = elf();
        fs::write(&path, &bytes).unwrap();
        assert!(read_override(&path, &inputs)
            .unwrap_err()
            .to_string()
            .contains("requires its build record"));
        let record = manifest(&inputs, &bytes).unwrap();
        fs::write(sidecar(&path), serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(
            read_override(&path, &inputs).unwrap(),
            (bytes.clone(), record)
        );
        let mut changed = inputs.clone();
        changed.source_sha256 = "different-source".into();
        assert!(read_override(&path, &changed).is_err());
        changed = inputs.clone();
        changed.recipe.go_version = "go1.26.1".into();
        assert!(read_override(&path, &changed).is_err());
        let mut tampered = bytes;
        tampered[40] = 1;
        fs::write(&path, &tampered).unwrap();
        assert!(read_override(&path, &inputs)
            .unwrap_err()
            .to_string()
            .contains("digest/size"));
        assert!(manifest(
            &Inputs {
                recipe: Recipe::new("aarch64", "v26.4.25", "b4f0898").unwrap(),
                ..inputs
            },
            &tampered
        )
        .is_err());
    }

    #[test]
    fn identical_inputs_with_changed_hash_fail_without_replacing_baseline() {
        let directory = tempfile::tempdir().unwrap();
        let original = manifest(&test_inputs(), &elf()).unwrap();
        ledger(directory.path(), |ledger| ledger.remember(&original)).unwrap();
        let mut different = original.clone();
        different.binary_sha256 = "different-output".into();
        let before = fs::read(directory.path().join("records.json")).unwrap();
        assert!(
            ledger(directory.path(), |ledger| ledger.remember(&different))
                .unwrap_err()
                .to_string()
                .contains("identical inputs")
        );
        assert_eq!(
            fs::read(directory.path().join("records.json")).unwrap(),
            before
        );
        assert!(ledger(directory.path(), |ledger| ledger.check(&original)).unwrap());
        different.inputs.source_sha256 = "new-inputs".into();
        ledger(directory.path(), |ledger| ledger.remember(&different)).unwrap();
    }

    #[test]
    fn baseline_records_are_bounded_and_invalid_json_is_not_silently_reset() {
        let mut ledger = Ledger::default();
        let mut record = manifest(&test_inputs(), &elf()).unwrap();
        for index in 0..MAX_RECORDS + 5 {
            record.inputs.source_sha256 = index.to_string();
            ledger.remember(&record).unwrap();
        }
        assert_eq!(ledger.records.len(), MAX_RECORDS);
        assert_eq!(ledger.records[0].inputs.source_sha256, "5");
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("records.json"), b"broken").unwrap();
        assert!(super::ledger(directory.path(), |_| Ok(())).is_err());
    }

    #[test]
    fn concurrent_baseline_updates_preserve_all_records() {
        let directory = tempfile::tempdir().unwrap();
        thread::scope(|scope| {
            for index in 0..8 {
                let path = directory.path();
                scope.spawn(move || {
                    let mut record = manifest(&test_inputs(), &elf()).unwrap();
                    record.inputs.source_sha256 = index.to_string();
                    ledger(path, |ledger| ledger.remember(&record)).unwrap();
                });
            }
        });
        assert_eq!(
            ledger(directory.path(), |ledger| Ok(ledger.records.len())).unwrap(),
            8
        );
    }

    #[test]
    fn actual_go_graph_tracks_untracked_production_and_embedded_files_not_tests_or_docs() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path();
        fs::create_dir(source.join("main")).unwrap();
        fs::write(
            source.join("go.mod"),
            "module example.test/xray-build\n\ngo 1.26\n",
        )
        .unwrap();
        fs::write(source.join("go.sum"), "").unwrap();
        fs::write(source.join("main/main.go"), "package main\nimport _ \"embed\"\n//go:embed view.html\nvar view string\nfunc main() {}\n").unwrap();
        fs::write(source.join("main/view.html"), "first").unwrap();
        let go = PathBuf::from(std::env::var_os("BROCADE_GO").unwrap_or_else(|| "go".into()));
        recipe().check_toolchain(&go, source).unwrap();
        let initial = inputs(&go, source, &recipe()).unwrap();
        fs::write(source.join("main/README.md"), "docs").unwrap();
        fs::write(source.join("main/main_test.go"), "package main\n").unwrap();
        assert_eq!(initial, inputs(&go, source, &recipe()).unwrap());
        fs::write(source.join("main/view.html"), "second").unwrap();
        let embedded = inputs(&go, source, &recipe()).unwrap();
        assert_ne!(initial, embedded);
        fs::write(
            source.join("main/untracked.go"),
            "package main\nvar added = 1\n",
        )
        .unwrap();
        assert_ne!(embedded, inputs(&go, source, &recipe()).unwrap());
    }

    #[cfg(unix)]
    fn fake_go(source: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let go = source.join("go");
        fs::write(source.join("go.mod"), "module fixture\n").unwrap();
        fs::write(source.join("go.sum"), "").unwrap();
        fs::write(source.join("main.go"), "package main\n").unwrap();
        fs::write(source.join("binary"), elf()).unwrap();
        fs::write(
            &go,
            format!(
                r#"#!/bin/sh
set -eu
case "$1" in
  env) printf '%s\n' 'go{}';;
  list)
    case " $* " in *' -buildvcs=false '*) ;; *) exit 91;; esac
    printf '{{"Dir":"%s","ImportPath":"fixture","GoFiles":["main.go"]}}' "$PWD";;
  build)
    while [ "$1" != '-o' ]; do shift; done
    cp binary "$2"
    if [ -f alter-between-builds ]; then printf x >> binary; fi
    if [ -f alter-output ]; then printf x >> "$2"; fi
    if [ -f alter-source ]; then printf x >> main.go; fi
    ;;
  *) exit 1;;
esac
"#,
                GO_VERSION.trim()
            ),
        )
        .unwrap();
        fs::set_permissions(&go, fs::Permissions::from_mode(0o700)).unwrap();
        go
    }

    #[test]
    #[cfg(unix)]
    fn prepare_roundtrip_rejects_changed_output_stale_override_and_midbuild_edits() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path();
        let go = fake_go(source);
        let out = source.join("out");
        fs::create_dir(&out).unwrap();
        let baseline = source.join("ledger");
        let original = prepare(&go, source, &recipe(), &out, &baseline, None).unwrap();
        let prebuilt = out.join("xray-x86_64");
        assert_eq!(
            prepare(&go, source, &recipe(), &out, &baseline, Some(&prebuilt)).unwrap(),
            original
        );
        fs::write(source.join("alter-output"), "").unwrap();
        assert!(prepare(&go, source, &recipe(), &out, &baseline, None)
            .unwrap_err()
            .to_string()
            .contains("identical inputs"));
        assert_eq!(fs::read(&prebuilt).unwrap(), elf());
        fs::remove_file(source.join("alter-output")).unwrap();
        fs::write(source.join("main.go"), "changed").unwrap();
        assert!(prepare(&go, source, &recipe(), &out, &baseline, Some(&prebuilt)).is_err());
        fs::write(source.join("alter-source"), "").unwrap();
        assert!(prepare(&go, source, &recipe(), &out, &baseline, None)
            .unwrap_err()
            .to_string()
            .contains("inputs changed"));
        let mut wrong_toolchain = recipe();
        wrong_toolchain.go_version = "go0.0.0".into();
        assert!(wrong_toolchain
            .check_toolchain(&go, source)
            .unwrap_err()
            .to_string()
            .contains("requires exactly"));
    }

    #[test]
    #[cfg(unix)]
    fn unknown_prebuilt_manifest_is_not_trusted_without_an_independent_rebuild() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path();
        let go = fake_go(source);
        let before = inputs(&go, source, &recipe()).unwrap();
        let mut forged = elf();
        forged[50] = 1;
        let path = source.join("forged");
        fs::write(&path, &forged).unwrap();
        fs::write(
            sidecar(&path),
            serde_json::to_vec(&manifest(&before, &forged).unwrap()).unwrap(),
        )
        .unwrap();
        let error = prepare(
            &go,
            source,
            &recipe(),
            source,
            &source.join("ledger"),
            Some(&path),
        )
        .unwrap_err();
        assert!(error.to_string().contains("independent rebuild disagrees"));
    }

    #[test]
    #[cfg(unix)]
    fn first_seen_nondeterministic_build_never_becomes_a_baseline() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path();
        let go = fake_go(source);
        fs::write(source.join("alter-between-builds"), "").unwrap();
        let baseline = source.join("ledger");
        let error = prepare(&go, source, &recipe(), source, &baseline, None).unwrap_err();
        assert!(error.to_string().contains("independent rebuild disagrees"));
        assert_eq!(
            ledger(&baseline, |ledger| Ok(ledger.records.len())).unwrap(),
            0
        );
        assert!(!source.join("xray-x86_64").exists());
    }
}
