//! Which bytes this process is actually running.
//!
//! # Why not a version number
//!
//! Both places that used to answer this question answered it with something hand-written: the
//! User-Agent was the literal `brocade-agent/0.1.0`, and `NodeVersions::agent` was
//! `CARGO_PKG_VERSION`. Neither identifies a binary. Nobody bumps a workspace version on the way
//! to a node, so every build since the number was last touched claims to be the same thing —
//! which is precisely the question self-update has to answer ("am I the binary the control plane
//! wants?"), and precisely the question a fleet-wide rollout has to answer ("did this machine
//! actually take the new one?").
//!
//! The sha256 of the running binary answers both, and cannot drift from the truth the way a
//! number somebody has to remember to change does. It is also the same value the control plane
//! already computes at compile time for what it distributes (`build.rs`), so the two sides
//! compare without either of them converting anything.
//!
//! # Why it is read at startup and cached
//!
//! `/proc/self/exe` follows to the inode this process was started from, which is what "the bytes
//! I am running" means. After a self-update replaces the file that link still resolves to the old
//! inode (the kernel holds it open, the path merely reads `… (deleted)`), so a value computed
//! afterwards would still be this process's own — correct, but paid for again. Reading it once
//! costs one hash of a few megabytes at startup and nothing afterwards.
//!
//! # Why the failure is `None` rather than a fallback
//!
//! Falling back to `CARGO_PKG_VERSION` would put a value in the field that looks like an answer
//! and is not one — the control plane cannot tell it apart from a genuinely reported version, and
//! self-update would compare a sha against `0.1.0` forever without ever saying why. `None`
//! surfaces as the string `unknown`, which is a state somebody can look at and act on.
//!
//! In practice this only fails where `/proc` is not mounted; the agent is Linux-only and its
//! installer writes a systemd unit, so that is a broken machine rather than a supported one.

use std::sync::OnceLock;

use brocade_core::hash::sha256_hex;

/// What is reported when the binary cannot be hashed. Not a version, and deliberately not
/// shaped like one — anything parseable would eventually be read as one.
pub(crate) const UNKNOWN: &str = "unknown";

/// The sha256 of the running binary, lowercase hex, or `None` where `/proc/self/exe` cannot be
/// read.
pub(crate) fn self_sha256() -> Option<&'static str> {
    static SELF: OnceLock<Option<String>> = OnceLock::new();
    SELF.get_or_init(|| {
        std::fs::read("/proc/self/exe")
            .map_err(|error| {
                // Said once, at startup, rather than at every use: this is a fact about the
                // machine, and repeating it every cycle would bury the rounds that matter.
                eprintln!("identity: 读不了 /proc/self/exe（{error}）；自更新不会启动，版本会报 {UNKNOWN}");
            })
            .ok()
            .map(|bytes| sha256_hex(&bytes))
    })
    .as_deref()
}

/// The same value as a string that is always safe to report.
pub(crate) fn self_identity() -> &'static str {
    self_sha256().unwrap_or(UNKNOWN)
}

/// The architecture this binary was built for, in `uname -m`'s vocabulary.
///
/// The control plane keys what it distributes by that spelling (`EMBEDDED_AGENTS`, and the same
/// case branches `install.sh` selects with), while rust spells two of them differently. Mapping
/// here rather than at the far end keeps the wire in one vocabulary — the control plane should
/// not have to know which language its nodes were written in.
pub(crate) fn self_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test binary is a binary too, so this exercises the real path rather than a stub.
    #[test]
    fn the_running_binary_hashes_to_64_hex() {
        let sha = self_sha256().expect("测试自己也是个二进制，/proc/self/exe 一定读得到");
        assert_eq!(sha.len(), 64);
        assert!(sha
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    /// Cached, so that a caller in a loop does not re-read the binary each round.
    #[test]
    fn it_is_computed_once() {
        assert_eq!(self_sha256(), self_sha256());
    }

    /// It must equal what `sha256sum` on the file would say — the control plane computes its
    /// side with the same definition, and a difference in what is being hashed (the file versus
    /// anything derived from it) would make every comparison fail with nothing to point at.
    #[test]
    fn it_matches_hashing_the_file_at_that_path() {
        let path = std::fs::read_link("/proc/self/exe").expect("链接读得到");
        let bytes = std::fs::read(&path).expect("文件读得到");
        assert_eq!(self_sha256(), Some(sha256_hex(&bytes).as_str()));
    }
}
