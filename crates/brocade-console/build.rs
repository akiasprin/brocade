//! Compiling the agent binaries into the control plane — x86_64 and aarch64, both static.
//!
//! # Why bind them this way
//!
//! `/enroll/dist` tells a machine where to download the agent and what its sha256 is, and both
//! values used to be typed into env by the operator. That leaves a path everybody walks into:
//! change the agent, build the control plane, deploy it, and the URL still points at the previous
//! binary. The agent the machine installs holds nothing new while the console is all green — the
//! control plane does not know it is distributing a stale binary, which is the same class of
//! failure as "the control plane believes it knows what a machine looks like", brought forward to
//! before the release.
//!
//! The agent also happens to be the hardest link to update: it sits on other people's machines,
//! cannot be pushed to, and may not admit SSH either. So this cannot rest on memory; it has to rest
//! on the compiler.
//!
//! The binding is `include_bytes!`: the control-plane binary contains the agents it distributes.
//! Forgetting to rebuild the agent is then no longer a configuration problem discovered at runtime
//! but a compile error; and the sha256 cannot be wrong either, being computed from the same bytes.
//!
//! # Why embed rather than write an env file
//!
//! An env file does not solve the problem: the URL has to point at something really serving a
//! download, and build time does not know which domain the control plane will eventually answer on.
//! Embedded, the URL is assembled by the control plane at runtime
//! (`{BROCADE_AGENT_PUBLIC_URL}/brocade-agent/<arch>`), and build time need only produce the bytes
//! and the sha.
//!
//! The `BROCADE_AGENT_BIN_URL` / `_SHA256` environment variables still work and take precedence —
//! that route has to remain for a CDN, or for a fleet holding machines outside these two
//! architectures (armv7, riscv).
//!
//! # Why both architectures, and why they must build
//!
//! Building only the host architecture leaves an ARM machine installing a file that cannot run,
//! with `Exec format error` as the symptom — `install.sh` recognizes it and says so, but that means
//! manual intervention on every ARM machine, and manual intervention is what this machinery exists
//! to eliminate.
//!
//! A missing target fails the build rather than degrading to a single architecture: degrading would
//! silently ship a control plane able to install half the fleet, which is the very reason this file
//! exists.
//!
//! # Why a zig is now required (this used to read "no external toolchain needed")
//!
//! The agent reaches the control plane over https, which brought rustls + ring into the
//! dependencies, and ring has C and assembly and needs a C compiler. rustup's musl target ships
//! `libc.a` and crt but **no headers**, so rustup alone cannot build ring; clang cannot either, and
//! what is missing is that same set of headers.
//!
//! Replacing ring would preserve the zero-toolchain property, and both candidates are worse, as
//! measured:
//!
//! - `graviola`: pure Rust and zero toolchain, but it `assert!`s CPU features at every entry point
//!   (avx2/adx/bmi2 on x86_64, aes/pmull/sha2 on aarch64) and panics where they are absent. The
//!   agent installs on other people's machines, and cheap VPS virtualization frequently does not
//!   pass those flags through — which returns us to "the installed file cannot run", the same class
//!   of failure as the `Exec format error` above with the cause changed from the wrong architecture
//!   to insufficient CPU features. It also builds 1.3M larger than ring.
//! - `rustls-rustcrypto`: version 0.0.2-alpha, dragging in over 80 crates.
//!
//! So a build-time dependency is preferable. What matters is **which side pays**: installing zig
//! costs the build machine once and fails the build immediately when absent; graviola's cost is
//! spread across N of other people's machines, detonates at runtime, and is invisible. Not pushing
//! uncertainty onto the nodes is what this whole file does.
//!
//! zig rather than a musl cross-gcc: one installation covers both architectures, it ships musl
//! headers and libc, and it needs no root — unpack and use. It appears only at compile time; the
//! final link is still `rust-lld` against rustup's `libc.a`, and the artifact is still a static
//! binary with zero dynamic dependencies, requiring nothing installed on the node side.
//!
//! Note that this combination is somewhat non-standard: **the headers come from zig's musl and
//! `libc.a` from rustup's**. musl's ABI is very stable and ring uses only the most basic headers,
//! and it measures fine; but should either side changing musl versions produce something strange
//! one day, this is the first place to look. The fallback if it really breaks is to let zig link as
//! well (it ships a complete musl), at the cost of overriding that `rust-lld` entry in `TARGETS`.
//!
//! # Why release and static musl are fixed, and why a separate target directory
//!
//! - Fixed release: this binary is shipped to machines to run, not a build intermediate of the
//!   control plane. Nobody wants an agent with debug info installed on a node, and fixing it also
//!   has debug and release control planes distribute the same agent.
//! - Static musl: an agent dynamically linked against glibc is bound to the build machine's glibc
//!   version and lacks symbols on older distributions; on a musl system such as Alpine it does not
//!   run at all (`docs/preview.md` records this). Static removes the "which distribution" variable
//!   entirely.
//! - A separate target directory: the outer `cargo build` holds `target/`'s build lock, and
//!   starting another cargo against the same directory from build.rs deadlocks waiting on it (not
//!   slowly — forever).
//!
//! # The console front end travels the same way, for the same reason
//!
//! `frontend/` used to be served off disk: `vite build` wrote `frontend/dist`, and the control
//! plane read it at runtime through `BROCADE_CONSOLE_DIST`. That arrangement has the failure this
//! file exists to remove, only one layer up — nothing ties the bundle on disk to the source it was
//! built from. Deploy without rebuilding the front end and every API answer is current while the
//! page rendering them is a version old; the control plane cannot tell, because to it `dist/` is
//! just a directory. It also means shipping a release is two artifacts that have to arrive
//! together, and the second one is the easy one to forget.
//!
//! So the front end is built here and `include_bytes!`d as well: one file to copy to a machine, and
//! a stale bundle is once again a thing the compiler prevents rather than a thing an operator
//! remembers. `FRONTEND_WATCHED` is the same lesson as the dependency chain above — it has to list
//! everything whose change makes the bundle stale, and a missing entry is silent.
//!
//! The two escape hatches match the agent's. `BROCADE_CONSOLE_ASSETS_DIR` embeds a directory that
//! already exists and skips npm entirely (a CI job that built the front end in an earlier stage, or
//! a build machine with no node); and at *runtime* `BROCADE_CONSOLE_DIST` still serves off disk,
//! which is what makes it possible to iterate on the front end without recompiling the control
//! plane.
//!
//! Each asset is also gzipped at build time, because the control plane is frequently exposed
//! directly with no reverse proxy in front of it to do the compressing. It is 660K of bundle
//! against 190K compressed, over a link that on a first visit has nothing cached.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

/// One architecture to distribute.
struct Target {
    /// In `uname -m`'s terms, because `install.sh` ultimately selects embedded artifacts with
    /// `uname -m`.
    arch: &'static str,
    /// Go's spelling of the same architecture, used for the embedded Xray build.
    go_arch: &'static str,
    /// rust's triple.
    triple: &'static str,
    /// zig's spelling of the same target. **Not the same thing as the rust triple**: zig has no
    /// vendor field and rejects `x86_64-unknown-linux-musl` outright as an unknown system (the
    /// error is `UnknownOperatingSystem`), so both spellings must be kept rather than one derived
    /// from the other.
    zig_target: &'static str,
    /// aarch64's musl target defaults to the host's `cc` as its linker driver, which invokes
    /// x86_64's `ld` and reports `Relocations in generic ELF (EM: 183)`. Pointing at `rust-lld`
    /// sidesteps it. x86_64 links by default and needs no such pointer.
    linker: Option<&'static str>,
}

const TARGETS: &[Target] = &[
    Target {
        arch: "x86_64",
        go_arch: "amd64",
        triple: "x86_64-unknown-linux-musl",
        zig_target: "x86_64-linux-musl",
        linker: None,
    },
    Target {
        arch: "aarch64",
        go_arch: "arm64",
        triple: "aarch64-unknown-linux-musl",
        zig_target: "aarch64-linux-musl",
        linker: Some("-C linker=rust-lld"),
    },
];

/// The zig version is pinned for the same reason `rust-toolchain.toml` pins the toolchain: unpinned,
/// every build machine produces something slightly different, and this binary ships to the whole
/// fleet.
///
/// It is merely pinned more loosely — a mismatch warns rather than fails. zig's `cc` front end is
/// very stable, and stranding somebody on "you have 0.17 and I want 0.16" is a more concrete risk
/// than letting it through.
const ZIG_PINNED_VERSION: &str = "0.16.0";
/// The sha256 of that version's x86_64-linux package, for the install command in the error
/// message.
const ZIG_PINNED_SHA256: &str = "70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00";
/// The escape hatch naming where zig is.
const ZIG_ENV_VAR: &str = "BROCADE_ZIG";
/// Where to look when PATH does not have it. `~` expands against `HOME`.
const ZIG_FALLBACK_PATHS: &[&str] = &["~/.local/bin/zig", "~/.local/zig/zig", "/opt/zig/zig"];

/// The escape hatch naming a directory of already-built front-end files to embed as-is.
///
/// Deliberately *not* spelled `BROCADE_CONSOLE_DIST`: that name means "serve the console off this
/// directory" at runtime, and an operator who exports it in their shell for a local run would
/// otherwise silently change what the next `cargo build` embeds. Two different questions get two
/// different names.
const CONSOLE_ASSETS_ENV: &str = "BROCADE_CONSOLE_ASSETS_DIR";
/// The escape hatch naming where npm is, for a machine that keeps node outside PATH.
const NPM_ENV_VAR: &str = "BROCADE_NPM";
/// The vendored Xray baseline distributed to every new node.
const XRAY_VERSION: &str = "v26.4.25";
/// `git describe --always` for the upstream baseline, matching Xray's release workflow banner.
const XRAY_UPSTREAM_BUILD: &str = "b4f0898";
/// Xray source is part of this repository. The build must never clone or select an upstream tag.
const XRAY_SOURCE_DIR: &str = "third_party/xray-core";
/// Escape hatch for locating Go outside PATH.
const GO_ENV_VAR: &str = "BROCADE_GO";
/// Everything under `frontend/` whose change makes the built bundle stale, relative to the
/// workspace root. Missing one has the same shape as missing a crate in the agent's chain above:
/// build.rs does not re-run, the embedded bytes are the previous bundle, and every check is green.
///
/// Listed one by one rather than watching `frontend/` whole, because whole would pull in
/// `node_modules` (which `npm ci` rewrites, so the build script would dirty itself and re-run
/// forever) and `dist` (the developer's own, and not an input). The cost of the explicit list is
/// that **a new top-level directory under `frontend/` — `public/` being the one to expect — has to
/// be added here**, and until it is, changes inside it are invisible to the build.
const FRONTEND_WATCHED: &[&str] = &[
    "frontend/src",
    "frontend/index.html",
    "frontend/package.json",
    "frontend/package-lock.json",
    "frontend/vite.config.ts",
    "frontend/tsconfig.json",
];

fn main() {
    // Changed agent source has to rebuild, and what must be watched is the whole dependency chain
    // `agent -> deployment -> core`, not merely its own directory. The symptom of missing one has
    // been observed: change only core with not one word of agent source altered, build.rs does not
    // re-run, the embedded bytes are still the previous agent, compilation and deployment are all
    // green, and the sha `/enroll/dist` reports disagrees with the current source. Adding a
    // dependency means adding a line here.
    for watched in [
        "../brocade-agent/src",
        "../brocade-agent/Cargo.toml",
        "../brocade-probe/src",
        "../brocade-probe/Cargo.toml",
        "../brocade-deployment/src",
        "../brocade-deployment/Cargo.toml",
        "../brocade-core/src",
        "../brocade-core/Cargo.toml",
        "../../Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={watched}");
    }

    let workspace = workspace_root();
    let xray_source = workspace.join(XRAY_SOURCE_DIR);
    let xray_build_id = repository_build_id(&workspace);
    watch_tree(&xray_source);

    describe_build();
    println!("cargo:rustc-env=BROCADE_EMBEDDED_XRAY_VERSION={XRAY_VERSION}");
    println!("cargo:rustc-env=BROCADE_EMBEDDED_XRAY_BUILD_ID={xray_build_id}");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo 一定会给 OUT_DIR"));

    // Every precondition is checked before building starts. The other way round (checking while
    // building), a missing aarch64 prerequisite surfaces minutes after x86_64 finishes, when both
    // were knowable in the first second.
    let mut missing_targets = Vec::new();
    let mut needs_zig = false;
    let mut needs_go = false;
    for target in TARGETS {
        println!("cargo:rerun-if-env-changed={}", override_var(target.arch));
        println!(
            "cargo:rerun-if-env-changed={}",
            xray_override_var(target.arch)
        );
        println!("cargo:rerun-if-env-changed={}", cc_var(target.triple));
        if !env_set(&xray_override_var(target.arch)) {
            needs_go = true;
        }
        if env_set(&override_var(target.arch)) {
            continue;
        }
        // Whether the target is installed is checked first. Without it, what gets reported is a
        // string of linker errors or a missing std, several layers away from what actually needs
        // doing (running one rustup command).
        if !target_installed(target.triple) {
            missing_targets.push(target.triple);
        } else if !env_set(&cc_var(target.triple)) {
            // A build machine with its own `CC_<triple>` (a musl.cc toolchain, say) is left
            // alone, in the same spirit as `BROCADE_AGENT_BIN_*`: respect what is already
            // configured.
            needs_zig = true;
        }
    }
    // Found lazily: where everything takes an escape hatch, or both architectures bring their own
    // CC, a machine without zig builds all the same.
    let zig = if needs_zig { find_zig() } else { None };
    let go = if needs_go { find_go() } else { None };
    // Same laziness for npm: pointed at a directory of already-built files, node is nobody's
    // business.
    println!("cargo:rerun-if-env-changed={CONSOLE_ASSETS_ENV}");
    let needs_npm = !env_set(CONSOLE_ASSETS_ENV);
    let npm = if needs_npm { find_npm() } else { None };
    if !missing_targets.is_empty()
        || (needs_zig && zig.is_none())
        || (needs_go && go.is_none())
        || (needs_npm && npm.is_none())
    {
        panic!(
            "{}",
            missing_prerequisites(
                &missing_targets,
                needs_zig && zig.is_none(),
                needs_go && go.is_none(),
                needs_npm && npm.is_none(),
            )
        );
    }

    // Before the agents, and not because it matters more: it takes a couple of seconds where they
    // take minutes, so a front end that fails to typecheck says so almost immediately instead of
    // after two cross-compiles that were going to be thrown away.
    embed_console(&out_dir, npm.as_deref());

    for target in TARGETS {
        let built = match env::var(xray_override_var(target.arch)) {
            Ok(path) if !path.trim().is_empty() => {
                let path = PathBuf::from(path.trim());
                println!("cargo:rerun-if-changed={}", path.display());
                path
            }
            _ => build_xray(
                &out_dir,
                &xray_source,
                target,
                go.as_deref().expect("前置检查保证 Go 存在"),
                &xray_build_id,
            ),
        };
        embed_binary(
            &out_dir,
            &built,
            "xray",
            target.arch,
            "BROCADE_EMBEDDED_XRAY_SHA256",
        );
    }

    for target in TARGETS {
        let built = match env::var(override_var(target.arch)) {
            // For cross-compiling with another toolchain, or embedding a file that already
            // exists. It goes through the sha computation below all the same — which cannot
            // guarantee the architecture is right, but does guarantee the bytes served and the sha
            // reported are one and the same.
            Ok(path) if !path.trim().is_empty() => {
                let path = PathBuf::from(path.trim());
                // Watch the file itself, not merely the environment variable's value. Where the
                // path is unchanged and the file is replaced (rebuilding an agent over it, which
                // is exactly what an upgrade does), `rerun-if-env-changed` alone notices nothing:
                // build.rs does not re-run and the embedded bytes are still the previous version.
                // The same class of failure as missing brocade-core above, and equally silent.
                println!("cargo:rerun-if-changed={}", path.display());
                path
            }
            _ => build_agent(&out_dir, target, zig.as_deref()),
        };
        embed_binary(
            &out_dir,
            &built,
            "brocade-agent",
            target.arch,
            "BROCADE_EMBEDDED_AGENT_SHA256",
        );
    }
}

/// Accumulate one error stating everything that is missing at once.
///
/// Reported in two rounds, somebody adds the target, runs again, and only then learns zig is
/// missing too — when both were knowable in the first second. Each line carries a command that can
/// be pasted directly: whoever reads this file is stuck unable to build, and sending them off to
/// search for installation instructions is indefensible.
fn missing_prerequisites(
    missing_targets: &[&str],
    missing_zig: bool,
    missing_go: bool,
    missing_npm: bool,
) -> String {
    let mut text = String::from("\n控制面编不出来，缺这些前置条件：\n");
    let mut n = 0;
    let mut next = move || {
        n += 1;
        n
    };

    if !missing_targets.is_empty() {
        text.push_str(&format!(
            "\n[{}] 交叉编译 target：{}\n\n\
             控制面要把这几个架构的 agent 都带上，只带一个的话另一半机队装下来是个\n\
             跑不动的文件（Exec format error），每台都得人工补一次。\n\n\
             补上：\n{}\n",
            next(),
            missing_targets.join("、"),
            missing_targets
                .iter()
                .map(|triple| format!("  rustup target add {triple}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }

    if missing_zig {
        text.push_str(&format!(
            "\n[{}] zig {ZIG_PINNED_VERSION}（agent 依赖的 ring 有 C 和汇编，要有 C 编译器；\n    \
             rustup 的 musl target 只带 libc.a 和 crt，不带头文件——理由见本文件开头）\n\n\
             解压即用，不需要 root，也不需要装进系统：\n\n\
             \x20 curl -fsSLO https://ziglang.org/download/{ZIG_PINNED_VERSION}/zig-x86_64-linux-{ZIG_PINNED_VERSION}.tar.xz\n\
             \x20 echo '{ZIG_PINNED_SHA256}  zig-x86_64-linux-{ZIG_PINNED_VERSION}.tar.xz' | sha256sum -c -\n\
             \x20 tar xf zig-x86_64-linux-{ZIG_PINNED_VERSION}.tar.xz -C ~/.local/\n\
             \x20 ln -sf ~/.local/zig-x86_64-linux-{ZIG_PINNED_VERSION}/zig ~/.local/bin/zig\n\n\
             \x20 （构建机是 ARM 的话把文件名里的 x86_64 换成 aarch64，sha256 去\n\
             \x20   https://ziglang.org/download/ 对）\n\n\
             不校验 sha256 就装的话，这条路等于「从网上下点东西然后拿它编要发给\n\
             全机队的 root 程序」——install.sh 顶上拒绝过同一件事。\n\n\
             找的位置依次是：${ZIG_ENV_VAR}、PATH 里的 zig、{}\n",
            next(),
            ZIG_FALLBACK_PATHS.join("、"),
        ));
    }

    if missing_go {
        text.push_str(&format!(
            "\n[{}] Go 1.26（用仓库内的 {XRAY_SOURCE_DIR} 构建 Brocade Xray）\n\n\
             装好 Go 1.26 后确保 `go version` 能运行，或者用 {GO_ENV_VAR} 指定完整路径。\n\
             Xray 源码已经钉在仓库内，构建过程不会 clone 或切换上游仓库。\n",
            next(),
        ));
    }

    if missing_npm {
        text.push_str(&format!(
            "\n[{}] npm（控制面把控制台前端也编进来了，理由见本文件开头）\n\n\
             装 node 20 以上就行，发行版的包最省事：\n\n\
             \x20 apt install nodejs npm   /   dnf install nodejs npm   /   brew install node\n\n\
             \x20 （发行版的版本太旧就去 https://nodejs.org/dist 取预编译包，解压即用，\n\
             \x20   同目录有 SHASUMS256.txt，校验一下——编出来的东西是要发出去的）\n\n\
             找的位置依次是：${NPM_ENV_VAR}、PATH 里的 npm\n\n\
             构建机上不想有 node 的话（CI 前一步已经编好、或者交叉构建），\n\
             把编好的目录用 {CONSOLE_ASSETS_ENV} 指过去，这一步就整个跳过：\n\n\
             \x20 {CONSOLE_ASSETS_ENV}=/path/to/frontend/dist cargo build --release\n",
            next(),
        ));
    }

    text.push_str(&format!(
        "\n确实只想带部分架构、或者要嵌现成的文件，agent 用 {}，Xray 用 {} 指过去。\n\
         手上已经有 musl 交叉工具链的，设 {} 就不会再要 zig。\n",
        TARGETS
            .iter()
            .map(|t| override_var(t.arch))
            .collect::<Vec<_>>()
            .join(" / "),
        TARGETS
            .iter()
            .map(|t| xray_override_var(t.arch))
            .collect::<Vec<_>>()
            .join(" / "),
        TARGETS
            .iter()
            .map(|t| cc_var(t.triple))
            .collect::<Vec<_>>()
            .join(" / "),
    ));
    text
}

/// The escape hatch naming which file to embed for one architecture, such as
/// `BROCADE_AGENT_BIN_AARCH64`.
fn override_var(arch: &str) -> String {
    format!("BROCADE_AGENT_BIN_{}", arch.to_uppercase())
}

/// The escape hatch naming which prebuilt Xray file to embed for one architecture.
fn xray_override_var(arch: &str) -> String {
    format!("BROCADE_XRAY_BIN_{}", arch.to_uppercase())
}

/// The per-target compiler variable cc-rs recognizes, such as
/// `CC_aarch64_unknown_linux_musl`. Its presence says the build machine has its own cross
/// toolchain, and zig is then none of our business.
fn cc_var(triple: &str) -> String {
    format!("CC_{}", triple.replace('-', "_"))
}

/// The same for `ar`. cc-rs needs both, and given only CC it takes the host's `ar`, producing a
/// static library in x86_64 format whose symptom appears only at the link step.
fn ar_var(triple: &str) -> String {
    format!("AR_{}", triple.replace('-', "_"))
}

/// The environment variable exists and is not an empty string. An empty string always counts as
/// unset here: `FOO= cargo build` is the usual way of turning FOO off temporarily, and treating it
/// as an empty path detonates inexplicably further down.
fn env_set(name: &str) -> bool {
    env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

/// Resolve a command name to an absolute path through `PATH`.
///
/// Without invoking `which` or `command -v`: that is another process, and `which` is not installed
/// on every slim system — depending on another external command in order to find a compiler has it
/// backwards.
fn resolve_in_path(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Find a usable Go toolchain. Xray's go.mod pins the required language version; the build below
/// sets GOTOOLCHAIN=local so an old binary fails rather than downloading a different compiler.
fn find_go() -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed={GO_ENV_VAR}");

    let mut candidates = Vec::new();
    if let Ok(explicit) = env::var(GO_ENV_VAR) {
        if !explicit.trim().is_empty() {
            candidates.push(PathBuf::from(explicit.trim()));
        }
    }
    candidates.extend(resolve_in_path("go"));

    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// Find a usable zig.
///
/// The test is that it runs rather than that the file exists, on the same consideration as
/// `target_installed` taking `rustc --print sysroot` instead of `rustup target list`: assume
/// nothing about how it was installed. A file that exists but cannot run (wrong architecture,
/// wrong permissions, a broken symlink) counts as absent, or the error is deferred to building
/// ring.
fn find_zig() -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed={ZIG_ENV_VAR}");

    let mut candidates = Vec::new();
    if let Ok(explicit) = env::var(ZIG_ENV_VAR) {
        if !explicit.trim().is_empty() {
            candidates.push(PathBuf::from(explicit.trim()));
        }
    }
    // The one on PATH, resolved to an absolute path here rather than passed on as a bare name: the
    // wrapper scripts exec through it, and the scripts run inside processes cc-rs starts, whose
    // PATH need not still be this one. A bare name working is a coincidence, and the symptom when
    // it breaks is "zig: not found" — two processes away from the real cause.
    candidates.extend(resolve_in_path("zig"));
    let home = env::var("HOME").unwrap_or_default();
    for path in ZIG_FALLBACK_PATHS {
        match path.strip_prefix("~/") {
            Some(rest) if !home.is_empty() => candidates.push(Path::new(&home).join(rest)),
            Some(_) => {}
            None => candidates.push(PathBuf::from(path)),
        }
    }

    let found = candidates.into_iter().find_map(|candidate| {
        let out = Command::new(&candidate).arg("version").output().ok()?;
        out.status.success().then(|| {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            (candidate, version)
        })
    });

    let (path, version) = found?;
    if version != ZIG_PINNED_VERSION {
        // It warns rather than blocks. Should something strange really happen, this line is the
        // first place to suspect.
        println!(
            "cargo:warning=zig 版本是 {version}，钉的是 {ZIG_PINNED_VERSION}（{}）。\
             agent 编得出来就没事，编不出来先看这里。",
            path.display()
        );
    }
    Some(path)
}

/// Write a pair of cc / ar wrapper scripts for a target, returning their paths.
///
/// # Why `zig cc` cannot be used as CC directly
///
/// Both were found by measurement, each after being stuck once:
///
/// - cc-rs appends `--target=<rust triple>` itself. zig does not accept that spelling — its targets
///   have no vendor field, and `x86_64-unknown-linux-musl` is judged `UnknownOperatingSystem` and
///   fails on the spot. So that argument is filtered out and the script supplies the target itself.
/// - cc-rs also adds `-nostdlibinc` for musl targets, assuming libc headers come from elsewhere
///   (rustup's self-contained arrangement). But zig's headers come precisely from its own sysroot,
///   and with that flag `stdint.h` cannot be found.
///
/// # Why generated rather than two hand-written scripts
///
/// Those two rules recorded in documentation eventually get written wrongly, and the symptom of
/// writing them wrongly is a screen of C compiler errors far from the cause. Written by build.rs
/// they grow alongside the `TARGETS` table and are not missed when an architecture is added.
fn zig_wrapper(out_dir: &Path, target: &Target, zig: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let write = |name: String, body: String| -> PathBuf {
        let path = out_dir.join(name);
        fs::write(&path, body)
            .unwrap_or_else(|error| panic!("写不了 zig 包装脚本 {}: {error}", path.display()));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|error| panic!("给 {} 加执行位失败: {error}", path.display()));
        path
    };

    let zig = zig.display();
    // `sh` rather than `bash`: whether the build machine has bash should not be a variable in
    // whether the control plane builds. The cost is no arrays, so the filtering below rotates the
    // arguments — take the first each time, append the ones to keep at the end, and after n
    // rotations `$@` holds only what should be kept.
    let cc = write(
        format!("zig-cc-{}", target.arch),
        format!(
            "#!/bin/sh\n\
             # 由 brocade-console/build.rs 生成，别手改（改了下次构建也会被覆盖）。\n\
             n=$#\n\
             i=0\n\
             while [ $i -lt $n ]; do\n\
             \x20   a=$1\n\
             \x20   shift\n\
             \x20   i=$((i + 1))\n\
             \x20   case \"$a\" in\n\
             \x20       --target=*|-nostdlibinc) continue ;;\n\
             \x20   esac\n\
             \x20   set -- \"$@\" \"$a\"\n\
             done\n\
             exec \"{zig}\" cc -target {} \"$@\"\n",
            target.zig_target
        ),
    );
    // ar needs no filtering, cc-rs only uses it to archive .o files. But it too must be zig's:
    // given only CC, cc-rs falls back to the host's `ar` and produces a static library in x86_64
    // format, whose symptom appears only at the link step as a "file in wrong format" that shows no
    // cause.
    let ar = write(
        format!("zig-ar-{}", target.arch),
        format!(
            "#!/bin/sh\n\
             # 由 brocade-console/build.rs 生成，别手改。\n\
             exec \"{zig}\" ar \"$@\"\n"
        ),
    );
    (cc, ar)
}

/// Whether this target's standard library is present.
///
/// It reads the directories in sysroot rather than running `rustup target list`: not every
/// toolchain is managed by rustup, whereas the sysroot layout is rustc's own business and
/// `rustc --print sysroot` always answers.
fn target_installed(triple: &str) -> bool {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let Ok(out) = Command::new(rustc).arg("--print").arg("sysroot").output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Path::new(&sysroot)
        .join("lib/rustlib")
        .join(triple)
        .join("lib")
        .is_dir()
}

fn build_xray(
    out_dir: &Path,
    source: &Path,
    target: &Target,
    go: &Path,
    build_id: &str,
) -> PathBuf {
    let output = out_dir.join(format!("xray-build-{}", target.arch));
    let ldflags = format!("-X github.com/xtls/xray-core/core.build={build_id} -s -w -buildid=");
    let status = Command::new(go)
        .args([
            "build",
            "-mod=readonly",
            "-trimpath",
            "-buildvcs=false",
            "-gcflags=all=-l=4",
            "-ldflags",
        ])
        .arg(ldflags)
        .arg("-o")
        .arg(&output)
        .arg("./main")
        .current_dir(source)
        .env("CGO_ENABLED", "0")
        .env("GOOS", "linux")
        .env("GOARCH", target.go_arch)
        // Never make a Console build silently download and switch to another Go toolchain.
        .env("GOTOOLCHAIN", "local")
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "起不了 Go 去编 {} 的 Brocade Xray（源码 {}）：{error}",
                target.arch,
                source.display()
            )
        });
    if !status.success() {
        panic!(
            "编不出 {} 的 Brocade Xray {XRAY_VERSION}（退出码 {:?}）。\n\
             源码必须来自仓库内的 {XRAY_SOURCE_DIR}，不会回退到社区发行包。",
            target.arch,
            status.code()
        );
    }
    output
}

/// Keep Xray's upstream banner shape while identifying the Brocade source revision that produced
/// the embedded binary. A tarball build has no repository metadata and falls back to the imported
/// upstream baseline.
fn repository_build_id(workspace: &Path) -> String {
    let commit = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|value| !value.is_empty());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| !out.stdout.is_empty());

    match (commit, dirty) {
        (Some(commit), true) => format!("{commit}-dirty"),
        (Some(commit), false) => commit,
        (None, _) => XRAY_UPSTREAM_BUILD.to_owned(),
    }
}

fn build_agent(out_dir: &Path, target: &Target, zig: Option<&Path>) -> PathBuf {
    let triple = target.triple;
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    // Placed under OUT_DIR: `cargo clean` clears it along with everything else and it does not
    // contend for the outer target's lock. Both architectures share this one directory, so common
    // dependencies are unpacked once.
    let target_dir = out_dir.join("agent-target");
    let workspace = workspace_root();

    let mut command = Command::new(&cargo);
    command
        .args(["build", "--release", "-p", "brocade-agent", "--target"])
        .arg(triple)
        .arg("--manifest-path")
        .arg(workspace.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        // Run from the workspace root rather than build.rs's own directory: cargo gives rustc
        // relative paths only where the source sits beneath the current directory, and otherwise
        // absolute paths are stamped into the binary through `file!()`. The consequences are, one,
        // the build machine's directory structure shipping to every node, and two, artifacts that
        // are no longer reproducible — an operator's own build produces a sha disagreeing with what
        // the control plane reports, leaving nobody able to verify whether what is distributed is
        // the agent in the source. And this binary runs as root on every node.
        .current_dir(&workspace)
        // Those inherited from the outer build would have the nested cargo touch the outer target
        // directory, or inherit a RUSTFLAGS meant for the control plane (which may hold things true
        // only of the host architecture). All of them are stripped.
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET");
    if let Some(flags) = target.linker {
        // The per-target variable rather than a global RUSTFLAGS: only this one target needs a
        // different linker, and setting it globally would affect the other architecture's build
        // too.
        command.env(
            format!(
                "CARGO_TARGET_{}_RUSTFLAGS",
                triple.to_uppercase().replace('-', "_")
            ),
            flags,
        );
    }
    // Prepare a compiler for ring's C and assembly. Where the build machine brings its own
    // `CC_<triple>`, not one word is touched — that configuration was set deliberately, and
    // overriding it treats "I have my own toolchain" as unsaid.
    if let (Some(zig), false) = (zig, env_set(&cc_var(triple))) {
        let (cc, ar) = zig_wrapper(out_dir, target, zig);
        command.env(cc_var(triple), cc).env(ar_var(triple), ar);
    }

    let status = command
        .status()
        .unwrap_or_else(|error| panic!("起不了 cargo 去编 {triple} 的 agent：{error}"));
    if !status.success() {
        panic!(
            "编不出 {triple} 的 brocade-agent（退出码 {:?}）。\n\
             控制面分发的就是这个二进制，编不出来就不该编出一个会分发过期版本的控制面。",
            status.code()
        );
    }
    target_dir.join(triple).join("release/brocade-agent")
}

fn embed_binary(out_dir: &Path, built: &Path, name: &str, arch: &str, sha_env_prefix: &str) {
    let bytes = fs::read(built)
        .unwrap_or_else(|error| panic!("读不了 {name} 二进制 {}: {error}", built.display()));
    if bytes.is_empty() {
        panic!("{name} 二进制是空的：{}", built.display());
    }
    fs::write(out_dir.join(format!("{name}-{arch}")), &bytes).expect("写不进 OUT_DIR");
    println!(
        "cargo:rustc-env={sha_env_prefix}_{}={}",
        arch.to_uppercase(),
        sha256(&bytes)
    );
}

/// `sha2` is in the dependency graph anyway (both agent and core use it), and there is no reason to
/// hand-write SHA-256 in a build script for one digest — a wrong value here costs the whole fleet
/// its agent, and hand-written implementations fail in exactly the way that gets most inputs
/// right.
fn sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// What a person can read about the agents this build carries: the version somebody maintains, and
/// the commit it came from.
///
/// # Why this is not compiled into the agent
///
/// It would be the obvious place, and it is the wrong one. Baking a commit id into the agent makes
/// the commit part of the bytes, so every commit — including one that only touches documentation —
/// produces a different agent sha, a different release id, and a fleet-wide upgrade that changes
/// no behaviour. It would also cost the reproducibility this file goes out of its way to preserve
/// elsewhere (see the `current_dir` passage): an operator building the same source could no longer
/// arrive at the same sha unless they were on the same commit with the same dirty state.
///
/// So the identity of the *bytes* stays the sha256 of the bytes, and this rides alongside as
/// metadata about the control plane that serves them. The console records it when somebody
/// releases, which is the moment the pairing matters.
fn describe_build() {
    // Every crate inherits `[workspace.package].version`, so the console's own version is also the
    // agent's — one number, maintained in one manifest. An earlier revision of this function
    // parsed the agent's `Cargo.toml` to read it separately, which was machinery for keeping two
    // numbers in sync that are now the same number.
    println!("cargo:rerun-if-changed=../../Cargo.toml");

    // Absent git, or built from a tarball, this is simply unknown — not a build failure. The sha256
    // is what identifies the artifact; this is here to be read.
    let commit = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|id| !id.is_empty());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| !out.stdout.is_empty());
    let described = match (commit, dirty) {
        // Said plainly rather than with a `+` or `*` suffix somebody has to know the convention
        // for. A dirty build is the one case where the commit does not describe the bytes, and
        // that is worth a word rather than a symbol.
        (Some(id), true) => format!("{id}-改动未提交"),
        (Some(id), false) => id,
        (None, _) => "unknown".to_owned(),
    };
    println!("cargo:rustc-env=BROCADE_AGENT_COMMIT={described}");

    // Without this the commit is frozen at whatever it was the first time this ran. `.git/HEAD`
    // covers checkouts and commits on a branch; the ref file covers committing without moving HEAD.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    if let Ok(head) = fs::read_to_string("../../.git/HEAD") {
        if let Some(reference) = head.strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=../../.git/{}", reference.trim());
        }
    }
}

/// The workspace root, from where cargo says this crate is.
fn workspace_root() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo 一定会给"))
        .join("../..")
        .canonicalize()
        .expect("工作区根目录一定在")
}

/// Watch the complete vendored source tree, including directories so newly added files also rerun
/// the build script. Xray updates are source updates; a stale embedded binary must not survive one.
fn watch_tree(root: &Path) {
    fn visit(path: &Path) {
        println!("cargo:rerun-if-changed={}", path.display());
        if !path.is_dir() {
            return;
        }
        let mut entries = fs::read_dir(path)
            .unwrap_or_else(|error| panic!("读不了 vendored source {}: {error}", path.display()))
            .map(|entry| entry.expect("读 vendored source 目录项"))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            visit(&entry.path());
        }
    }

    if !root.join("go.mod").is_file() || !root.join("LICENSE").is_file() {
        panic!(
            "Brocade Xray 源码不完整：{} 必须同时包含 go.mod 和 LICENSE",
            root.display()
        );
    }
    visit(root);
}

/// Find a usable npm, on the same terms as `find_zig`: the test is that it runs, not that a file
/// exists. node is commonly installed through nvm/fnm, where `npm` is a symlink into a version
/// directory that a `nvm uninstall` can leave dangling.
fn find_npm() -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed={NPM_ENV_VAR}");

    let mut candidates = Vec::new();
    if let Ok(explicit) = env::var(NPM_ENV_VAR) {
        if !explicit.trim().is_empty() {
            candidates.push(PathBuf::from(explicit.trim()));
        }
    }
    candidates.extend(resolve_in_path("npm"));

    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// Run an external command, or fail the build saying which one and why it mattered.
fn run(command: &mut Command, what: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("起不了 {what}：{error}"));
    if !status.success() {
        panic!(
            "{what} 失败（退出码 {:?}）。\n\
             控制台前端是编进控制面里的，前端编不出来就不该编出一个带着上个版本前端的控制面。",
            status.code()
        );
    }
}

/// Whether `built` is at least as new as `source`. Absent or unreadable counts as stale — the
/// conservative direction, since being wrong here means rebuilding something that did not need it.
fn newer_than(built: &Path, source: &Path) -> bool {
    let modified = |path: &Path| path.metadata().and_then(|meta| meta.modified()).ok();
    match (modified(built), modified(source)) {
        (Some(built), Some(source)) => built >= source,
        _ => false,
    }
}

/// Produce `frontend/dist`'s contents, returning where they landed.
///
/// The output goes to OUT_DIR rather than `frontend/dist`: a build script writing into the source
/// tree makes two concurrent `cargo build`s (debug and release, say) overwrite each other's bundle
/// mid-read, and it would fight with a `npm run build` the developer runs by hand. `frontend/dist`
/// stays entirely the developer's, for `BROCADE_CONSOLE_DIST` to serve off.
///
/// `npm run build` rather than `vite build` directly, so that "how the front end is built" has one
/// definition and it is `package.json`'s. That script also runs `tsc --noEmit`, which means a
/// type error in the front end fails `cargo build` — intended: the bundle is part of this binary
/// now, and a binary should not be produced around a front end that does not typecheck.
fn build_frontend(out_dir: &Path, npm: &Path) -> PathBuf {
    let workspace = workspace_root();
    for watched in FRONTEND_WATCHED {
        println!(
            "cargo:rerun-if-changed={}",
            workspace.join(watched).display()
        );
    }

    let frontend = workspace.join("frontend");
    // `npm ci` is what guarantees node_modules matches the lockfile, but it deletes and reinstalls
    // every time, and paying that on every front-end source change would be absurd. npm writes
    // `node_modules/.package-lock.json` on install describing what it put there, so its age against
    // the lockfile answers exactly the question being asked — no stamp of our own in OUT_DIR, which
    // would also wrongly reinstall after a `cargo clean`.
    let lockfile = frontend.join("package-lock.json");
    let installed = frontend.join("node_modules/.package-lock.json");
    if !newer_than(&installed, &lockfile) {
        run(
            Command::new(npm)
                .args(["ci", "--no-audit", "--no-fund"])
                .current_dir(&frontend),
            "npm ci",
        );
    }

    let dist = out_dir.join("console-dist");
    run(
        Command::new(npm)
            .args(["run", "build", "--", "--outDir"])
            .arg(&dist)
            // vite refuses to clear an outDir outside its root unless told to. Without clearing,
            // a bundle whose content hash changed leaves the previous `assets/index-<hash>.js`
            // behind and it gets embedded too — dead weight in the binary that nothing references.
            .arg("--emptyOutDir")
            .current_dir(&frontend),
        "npm run build",
    );
    dist
}

/// Build the console front end and write the table `src/assets.rs` includes.
fn embed_console(out_dir: &Path, npm: Option<&Path>) {
    let dist = match env::var(CONSOLE_ASSETS_ENV) {
        Ok(dir) if !dir.trim().is_empty() => {
            let dir = PathBuf::from(dir.trim());
            // Watch the directory, not merely the variable's value: rebuilding the front end in
            // place leaves the path unchanged, and `rerun-if-env-changed` alone would embed the
            // previous bundle. Same failure as the agent override's, and equally silent.
            println!("cargo:rerun-if-changed={}", dir.display());
            dir
        }
        _ => build_frontend(out_dir, npm.expect("没有 npm 的话前面就 panic 了")),
    };

    let mut files = Vec::new();
    collect_files(&dist, &dist, &mut files);
    // read_dir's order is whatever the filesystem says, and an embedded table that reorders between
    // builds makes two builds of the same source produce different binaries for no reason.
    files.sort();
    if !files.iter().any(|(route, _)| route == "/index.html") {
        panic!(
            "{} 里没有 index.html，这不是一份能用的前端产物（找到 {} 个文件）",
            dist.display(),
            files.len()
        );
    }

    let mut code = String::from(
        "// 由 brocade-console/build.rs 生成，别手改（改了下次构建也会被覆盖）。\n\
         pub static CONSOLE_ASSETS: &[ConsoleAsset] = &[\n",
    );
    for (route, path) in &files {
        let bytes = fs::read(path)
            .unwrap_or_else(|error| panic!("读不了前端产物 {}: {error}", path.display()));
        // Compressed at build time rather than per request: these bytes never change for the life
        // of the binary, so compressing them once at the highest level is strictly better than
        // compressing them again for every visitor at a level chosen to be cheap.
        let gzipped = gzip(&bytes);
        // A file that barely compresses (already-compressed images, tiny files whose gzip header
        // outweighs the saving) carries its compressed copy for nothing — it would sit in the
        // binary and never be the smaller answer.
        let gzip_expr = if gzipped.len() * 10 < bytes.len() * 9 {
            let gz_path = out_dir
                .join("console-gzip")
                .join(route.trim_start_matches('/'));
            fs::create_dir_all(gz_path.parent().expect("route 至少有一层"))
                .expect("写不进 OUT_DIR");
            fs::write(&gz_path, &gzipped).expect("写不进 OUT_DIR");
            format!("Some(include_bytes!({:?}))", path_str(&gz_path))
        } else {
            "None".to_owned()
        };
        code.push_str(&format!(
            "    ConsoleAsset {{\n        \
                 path: {:?},\n        \
                 content_type: {:?},\n        \
                 bytes: include_bytes!({:?}),\n        \
                 gzip: {gzip_expr},\n    \
             }},\n",
            route,
            content_type(route),
            path_str(path),
        ));
    }
    code.push_str("];\n");
    fs::write(out_dir.join("console_assets.rs"), code).expect("写不进 OUT_DIR");
}

/// Every file under `root`, as `(the URL path it answers on, where it is on disk)`.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("读不了前端产物目录 {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("目录项读得出来").path();
        if path.is_dir() {
            collect_files(root, &path, out);
            continue;
        }
        let relative = path.strip_prefix(root).expect("是从 root 走下来的");
        // Built on the URL's separator rather than the platform's. They agree on unix and this
        // only ever runs on unix, but a path joined with the wrong one is the kind of thing that
        // silently 404s every asset.
        let route = format!(
            "/{}",
            relative
                .components()
                .map(|part| part.as_os_str().to_str().expect("前端产物的文件名是 UTF-8"))
                .collect::<Vec<_>>()
                .join("/")
        );
        // The serving side matches the raw request path against this string, without decoding
        // (`assets.rs` argues why). So a name the browser would percent-encode — a space, a CJK
        // character — arrives spelled `/%E5%9B%BE.png` and misses a table holding `/图.png`: a
        // 404 on a file that is demonstrably embedded, which is about the least explicable symptom
        // available. Refused here instead, where it is one sentence and one rename.
        if let Some(bad) = route
            .chars()
            .find(|c| !c.is_ascii_alphanumeric() && !"/-._~".contains(*c))
        {
            panic!(
                "前端产物里有个文件名带 {bad:?}，浏览器请求它时会转义，服务端按原样查表就撞不上：\n  \
                 {route}\n\n\
                 改成只用 ASCII 字母数字和 - . _ ~。vite 自己产出的名字都满足，\n\
                 这条一般是 frontend/public/ 里手放的文件触发的。"
            );
        }
        out.push((route, path));
    }
}

/// A path as a string for `include_bytes!`. `{:?}` on the way out produces a valid Rust string
/// literal, so a directory name holding a quote or a backslash cannot break the generated file.
fn path_str(path: &Path) -> &str {
    path.to_str()
        .unwrap_or_else(|| panic!("路径不是 UTF-8：{}", path.display()))
}

/// What a browser should be told a file is.
///
/// Hand-written rather than pulled from `mime_guess`: this table covers what vite emits, and the
/// list is short enough that a dependency for it is not worth the build time. Anything unrecognized
/// is `application/octet-stream` — the browser downloads it rather than guessing, which for a
/// wrongly-typed script is the safer of the two failures.
fn content_type(route: &str) -> &'static str {
    // The extension of the last segment, not of the whole route: a dot in a directory name would
    // otherwise make `/assets.v2/app` look like it had an extension of `v2/app`.
    let name = route.rsplit('/').next().unwrap_or_default();
    match name
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        // Source maps are JSON, and so is anything vite copies from `public/`.
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).expect("写进 Vec 不会失败");
    encoder.finish().expect("写进 Vec 不会失败")
}
