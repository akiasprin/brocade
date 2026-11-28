# CI-aligned release environment

This directory defines the Ubuntu 22.04 / glibc 2.35 release ABI used by
`scripts/release_orchestrator.py`. Rust, Go and Zig versions are read from the
repository pins. Node must select an explicit 22.x release in the local
configuration; CI selects the Node 22 major version.

The orchestrator is a Linux control-host entry point. It creates four detached
worktrees (`base`, `cvm`, `cube-a`, `cube-b`), independent compilation caches,
and three Docker images. Jammy performs compilation with a glibc 2.35 ABI;
Debian 12 provides Python 3.11+, Git, `debugfs`, and `readelf` for independent
source identity checks, node packaging and verification. It does not deploy to production or modify
`/opt/brocade`.

## Local configuration

Keep the environment file and all evidence outside the repository. Start from
[environment.example.json](environment.example.json), replace every path and
SHA-256 with the selected immutable inputs, and keep the file private when it
contains local environment details.

- `source_repository` is an existing local Git checkout; `revision` selects one
  commit shared by all four build worktrees.
- `tools_directory` contains the host Linux toolchains mounted into Docker.
  Missing tools are downloaded only when `install_missing_tools` is true.
  Node, Go and Zig archives are SHA-256 checked before extraction.
- `cube_sysroot_libs` and `cube_bpf_include` are the fixed inputs required by
  `build-release.py --cube-runtime-only`. `cube_assets` contains the pinned
  Guest `manifest.json` and every file it names.
- OS, GeoIP and GeoSite inputs must be pinned explicitly for KVM acceptance.
- `run_directory` is the resumable acceptance directory. Do not reuse it for a
  different candidate.
- `stop_labs` stops only VMs owned by the completed suite. Retained disk
  images and private evidence are not deleted.
- Default requested parallelism is 16/16/12/12 jobs for the four independent
  builders. These are CPU-overcommit settings, not a guarantee that builds and
  all KVM suites can coexist: the default KVM reservation is 33 GiB with an
  8 GiB host reserve.

## Actions

Run each action with the same environment file:

```bash
python3 scripts/release_orchestrator.py prepare --environment /path/environment.json
python3 scripts/release_orchestrator.py preflight --environment /path/environment.json
python3 scripts/release_orchestrator.py build --environment /path/environment.json
python3 scripts/release_orchestrator.py verify --environment /path/environment.json
python3 scripts/release_orchestrator.py accept --environment /path/environment.json
```

`prepare` is intentionally not silently repeatable: completed attempt logs and
failed attempts are evidence. Choose a new evidence directory when rebuilding
the environment, and a new run directory when deliberately repeating a
mutating suite. Successful main phases and Stack scenarios resume without
re-execution. An interrupted Stack mutation or incomplete Agent-only first
install must be diagnosed and repeated in a new disposable lab; it is not
reported as a fresh-install pass by resuming a modified VM.

`verify` assembles both Cube bundles in the independent Python 3.11 packager,
reopens all four candidate source trees, checks ELF/glibc boundaries and Stack
A/B content identity, then freezes the deployment inputs. `accept` hashes those
frozen inputs again before starting any VM. It runs the release KVM scripts
from the same checkout as the orchestrator, without editing the candidate source
trees. Before starting the labs for a main-path run, it builds the static Linux
network probe in the pinned Docker compiler and records the source and binary
hashes. The deployment script validates and copies the probe and the pinned GeoIP
and GeoSite files. It creates the Agent-only output directory exclusively. After
lab preparation, the selected suites can run concurrently.

To explicitly retire only target caches marked by this orchestrator:

```bash
python3 scripts/release_orchestrator.py retire-targets --environment /path/environment.json
```

Unmarked caches and caches from a different image are never silently deleted;
the build refuses to mix them with the CI environment.

Evidence contains credentials, SSH identities and node tokens. Keep its
permissions private, do not commit it, and do not publish it. A successful build
only means `build_complete=true`; deployment remains `false` until the actual
installation and acceptance gates pass, and this workflow never authorizes a
production cutover.
