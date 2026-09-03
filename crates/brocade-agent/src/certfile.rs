//! Keeping this node's TLS certificate on disk.
//!
//! # Arrives in the desired response, not on its own cycle
//!
//! The certificate rides `DesiredStateResponse` — the `Certificate` variant when that is all the
//! node is owed, the `certificate` field of `Deployment` when a deployment is owed too. The
//! control plane judges the dimension on every desired poll (the node's reported sha against the
//! serving certificate), so there is no thread of our own and no endpoint of its own; the
//! fifteen-second apply round is the only cycle involved. `apply_material` below is called from
//! the apply path.
//!
//! The two earlier placements are recorded in `protocol.rs`, on `NodeCertificateMaterial`:
//! inside `NodeDesiredDeployment` (a converged machine receives 204, so a renewal waited for
//! the next release) and then a separate ten-minute poll (two channels could disagree, and a
//! machine could hold a config that references the certificate while its certificate channel
//! never ran). One channel with the check on every poll removes both.
//!
//! # Why absence never deletes
//!
//! A response without certificate material reaches here for reasons the agent cannot tell apart
//! — the fleet does not use certificates, this node's has not been issued yet, the control plane
//! was rolled back to a build without the check. Deleting on absence would let a control plane
//! that momentarily forgets take out a working TLS listener across the whole fleet. Keeping the
//! file costs nothing, and a certificate nobody renews expires on its own.
//!
//! Removal, when it is wanted, will be an explicit instruction — the same shape as everywhere else
//! here: "this item is off" is something the desired state says, not something absence implies.
//!
//! # Local reconcile does not cover it
//!
//! The idle-round local reconcile replays the artifacts already received, but it has no desired
//! certificate to compare against — the certificate is not an artifact, and its last state is
//! not kept in the state directory. A certificate file deleted by hand is therefore invisible to
//! the local check; the control plane sees the absence in the next runtime report, and the next
//! desired poll carries the material again.

use std::path::{Path, PathBuf};

use brocade_deployment::protocol::{CertificateObservation, NodeCertificateMaterial};

use crate::options::Options;
use crate::{create_private_dir, warn, write_private};

/// Under the state directory, which is already 0700 and already holds WireGuard and REALITY
/// private keys. A second private location would be a second thing to get the permissions right
/// on, and one of them would eventually be wrong.
const CERT_DIR: &str = "tls";
const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";

/// Apply certificate material received in the desired response.
///
/// Called from the apply path, for the `Certificate` variant and for the `certificate` field of
/// `Deployment` — before the deployment itself is applied, because the xray restart that applies
/// it tests the config, and a TLS ingress tests against the certificate file.
///
/// When the files already match, nothing is written and nothing is reported: the control plane
/// judged a difference from an observation that has not arrived yet, and the next report will
/// clear it.
pub(crate) fn apply_material(
    options: &Options,
    material: &NodeCertificateMaterial,
) -> Result<(), String> {
    if !write_certificate(Path::new(&options.state_dir), material)? {
        return Ok(());
    }

    // Say so at once rather than waiting for the next runtime round.
    //
    // What this machine holds is reported on the half-hourly cycle with versions and backlog,
    // which suited those: they change on nobody's schedule and mean nothing at finer resolution.
    // This one changes exactly here, and until it is reported the control plane keeps judging
    // the node as stale and repeats the material on every desired poll — fifteen seconds apart.
    let report_options = options.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("certificate-report".to_owned())
        .spawn(move || {
            if let Err(error) = crate::publish_runtime_now(&report_options) {
                warn(format!(
                    "certificate: 换好了，但报不上去（{error}）；下一轮会补"
                ));
            }
        })
    {
        warn(format!(
            "certificate: 换好了，但启动即时上报线程失败（{error}）；下一轮会补"
        ));
    }
    Ok(())
}

/// Writes the pair, or leaves the disk alone when it already matches.
///
/// Comparing first is not an optimization: these files hold a private key, and a write that
/// changes nothing is still a window where the file is truncated. It also keeps the mtime honest,
/// so anything that ever watches these files sees a change only when there was one.
pub(crate) fn write_certificate(
    state_dir: &Path,
    material: &NodeCertificateMaterial,
) -> Result<bool, String> {
    let dir: PathBuf = state_dir.join(CERT_DIR);
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);

    let unchanged = std::fs::read_to_string(&cert_path)
        .is_ok_and(|on_disk| on_disk == material.cert_pem)
        && std::fs::read_to_string(&key_path).is_ok_and(|on_disk| on_disk == material.key_pem);
    if unchanged {
        return Ok(false);
    }

    create_private_dir(&dir)?;
    // The key first. Between the two writes the pair is inconsistent either way, and a stale key
    // beside a new certificate fails loudly, where the other order would have the machine serve a
    // certificate whose key nobody holds.
    write_private(&key_path, &material.key_pem)?;
    write_private(&cert_path, &material.cert_pem)?;
    // Names, never contents: this line goes to a journal other people can read.
    eprintln!("证书已更新：{}", material.names.join(", "));
    Ok(true)
}

/// What is on disk, for the report the control plane compares against what it issued.
///
/// # Why this is worth a round trip
///
/// Without it the console shows what the control plane *issued*, which is not the same as what the
/// machine has. Three situations then look identical to "everything is fine": the write failed,
/// something else replaced the file, and the agent is too old to be managing certificates at all.
/// The last one is not hypothetical — every node in a fleet is in it until its agent is released.
///
/// Only a digest: the control plane holds the same bytes and can compare. Anything richer would
/// need an X.509 parser here, for facts the other side already knows.
pub(crate) fn observe(state_dir: &Path) -> CertificateObservation {
    let path = state_dir.join(CERT_DIR).join(CERT_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => CertificateObservation::Present {
            sha256: crate::sha256_hex(&bytes),
        },
        // Unreadable and absent are reported alike, deliberately. This runs as root, so the only
        // way to fail a read is that the file is not there in a usable sense — and inventing a
        // third state for a distinction nobody can act on would only make the page harder to read.
        Err(_) => CertificateObservation::Absent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(cert: &str, key: &str) -> NodeCertificateMaterial {
        NodeCertificateMaterial {
            names: vec![
                "*.a1b2.example.net".to_owned(),
                "a1b2.example.net".to_owned(),
            ],
            cert_pem: cert.to_owned(),
            key_pem: key.to_owned(),
        }
    }

    #[test]
    fn the_pair_lands_at_0600_under_a_0700_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("brocade-cert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        write_certificate(&dir, &material("CERT", "KEY")).unwrap();
        let tls = dir.join(CERT_DIR);
        assert_eq!(
            std::fs::read_to_string(tls.join(CERT_FILE)).unwrap(),
            "CERT"
        );
        assert_eq!(std::fs::read_to_string(tls.join(KEY_FILE)).unwrap(), "KEY");
        // The key is the node's identity for as long as the certificate lives; anything but 0600
        // hands it to every account on the machine.
        for file in [CERT_FILE, KEY_FILE] {
            let mode = std::fs::metadata(tls.join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{file} 的权限是 {mode:o}");
        }
        let dir_mode = std::fs::metadata(&tls).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writing_the_same_material_twice_does_not_touch_the_files() {
        let dir = std::env::temp_dir().join(format!("brocade-cert-same-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        write_certificate(&dir, &material("CERT", "KEY")).unwrap();
        let first = std::fs::metadata(dir.join(CERT_DIR).join(CERT_FILE))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_certificate(&dir, &material("CERT", "KEY")).unwrap();
        let second = std::fs::metadata(dir.join(CERT_DIR).join(CERT_FILE))
            .unwrap()
            .modified()
            .unwrap();
        // Equal mtimes mean the second call decided there was nothing to do. Rewriting identical
        // bytes every ten minutes would make the timestamp meaningless to anything watching it.
        assert_eq!(first, second);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_certificate_replaces_both_files() {
        let dir = std::env::temp_dir().join(format!("brocade-cert-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        write_certificate(&dir, &material("OLD", "OLDKEY")).unwrap();
        write_certificate(&dir, &material("NEW", "NEWKEY")).unwrap();
        let tls = dir.join(CERT_DIR);
        // Both, not one. A renewal issues a new key with the new certificate, and keeping either
        // half of the previous pair produces a listener that cannot start.
        assert_eq!(std::fs::read_to_string(tls.join(CERT_FILE)).unwrap(), "NEW");
        assert_eq!(
            std::fs::read_to_string(tls.join(KEY_FILE)).unwrap(),
            "NEWKEY"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
