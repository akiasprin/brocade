//! Keeping this node's TLS certificate on disk.
//!
//! # Arrives in the desired response, not on its own cycle
//!
//! Certificate pairs ride `DesiredStateResponse` — the `Certificates` variant when that is all the
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

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use brocade_deployment::protocol::{
    CertificateObservation, CertificatePairObservation, CertificateTrack, NodeCertificateMaterial,
    NodeCertificateSlotMaterial,
};

use crate::options::Options;
use crate::{create_private_dir, warn};

/// Under the state directory, which is already 0700 and already holds WireGuard and REALITY
/// private keys. A second private location would be a second thing to get the permissions right
/// on, and one of them would eventually be wrong.
const CERT_DIR: &str = "tls";
const PUBLIC_CA_DIR: &str = "public-ca";
const SELF_SIGNED_DIR: &str = "self-signed";
const SLOT_A_FILE: &str = "slot-a.pem";
const SLOT_B_FILE: &str = "slot-b.pem";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
) -> Result<bool, String> {
    if !write_certificate(Path::new(&options.state_dir), material)? {
        return Ok(false);
    }

    Ok(true)
}

/// Report only after the caller has made every running TLS consumer use the new files. Reporting
/// from the write itself made the control plane stop offering the certificate even when Xray's
/// reload failed, leaving the node permanently "current" on disk while serving the old leaf.
pub(crate) fn report_applied(options: &Options) {
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
    let dir = track_dir(state_dir, material.track);
    create_private_dir(&dir)?;
    let mut changed = false;
    for (file, slot) in [SLOT_A_FILE, SLOT_B_FILE].into_iter().zip(&material.slots) {
        let path = dir.join(file);
        let bundle = certificate_bundle(slot);
        if std::fs::read(&path).is_ok_and(|on_disk| on_disk == bundle) {
            continue;
        }
        atomic_write_private(&path, &bundle)?;
        changed = true;
    }
    if changed {
        // Names and ids, never contents: this line goes to a journal other people can read.
        eprintln!(
            "证书槽已更新（{}）：{} / {}",
            track_name(material.track),
            material.slots[0].certificate_id,
            material.slots[1].certificate_id
        );
    }
    Ok(changed)
}

fn track_name(track: CertificateTrack) -> &'static str {
    match track {
        CertificateTrack::PublicCa => "公有证书",
        CertificateTrack::SelfSigned => "自签证书",
    }
}

fn track_dir(state_dir: &Path, track: CertificateTrack) -> PathBuf {
    state_dir.join(CERT_DIR).join(match track {
        CertificateTrack::PublicCa => PUBLIC_CA_DIR,
        CertificateTrack::SelfSigned => SELF_SIGNED_DIR,
    })
}

fn certificate_bundle(slot: &NodeCertificateSlotMaterial) -> Vec<u8> {
    let mut bundle = Vec::with_capacity(slot.cert_pem.len() + slot.key_pem.len() + 2);
    bundle.extend_from_slice(slot.cert_pem.trim_end().as_bytes());
    bundle.push(b'\n');
    bundle.extend_from_slice(slot.key_pem.trim_end().as_bytes());
    bundle.push(b'\n');
    bundle
}

/// Replace one already-configured Xray path as a single filesystem operation. A certificate and
/// its private key share one PEM so a watcher can never observe a new half beside an old half.
fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("证书槽没有父目录：{}", path.display()))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.tmp-{}-{sequence}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("slot"),
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| format!("创建证书临时文件 {} 失败：{error}", temp.display()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("写入证书临时文件 {} 失败：{error}", temp.display()))?;
        std::fs::rename(&temp, path)
            .map_err(|error| format!("替换证书槽 {} 失败：{error}", path.display()))?;
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|error| format!("同步证书目录 {} 失败：{error}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
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
    CertificateObservation::Managed {
        public_ca: observe_track(&track_dir(state_dir, CertificateTrack::PublicCa)),
        self_signed: observe_track(&track_dir(state_dir, CertificateTrack::SelfSigned)),
    }
}

fn observe_track(dir: &Path) -> CertificatePairObservation {
    let digest = |file| {
        std::fs::read(dir.join(file))
            .ok()
            .map(|bytes| crate::sha256_hex(&bytes))
    };
    CertificatePairObservation {
        slot_a_sha256: digest(SLOT_A_FILE),
        slot_b_sha256: digest(SLOT_B_FILE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(id: &str, cert: &str, key: &str) -> NodeCertificateSlotMaterial {
        NodeCertificateSlotMaterial {
            certificate_id: id.to_owned(),
            names: vec![format!("{id}.example.com")],
            cert_pem: cert.to_owned(),
            key_pem: key.to_owned(),
        }
    }

    fn material(cert: &str, key: &str) -> NodeCertificateMaterial {
        NodeCertificateMaterial {
            track: CertificateTrack::SelfSigned,
            slots: [slot("a", cert, key), slot("b", cert, key)],
        }
    }

    #[test]
    fn the_pair_lands_at_0600_under_a_0700_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("brocade-cert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        write_certificate(&dir, &material("CERT", "KEY")).unwrap();
        let tls = dir.join(CERT_DIR).join(SELF_SIGNED_DIR);
        assert_eq!(
            std::fs::read_to_string(tls.join(SLOT_A_FILE)).unwrap(),
            "CERT\nKEY\n"
        );
        // The key is the node's identity for as long as the certificate lives; anything but 0600
        // hands it to every account on the machine.
        for file in [SLOT_A_FILE, SLOT_B_FILE] {
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
        let path = dir.join(CERT_DIR).join(SELF_SIGNED_DIR).join(SLOT_A_FILE);
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_certificate(&dir, &material("CERT", "KEY")).unwrap();
        let second = std::fs::metadata(&path).unwrap().modified().unwrap();
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
        let tls = dir.join(CERT_DIR).join(SELF_SIGNED_DIR);
        assert_eq!(
            std::fs::read_to_string(tls.join(SLOT_A_FILE)).unwrap(),
            "NEW\nNEWKEY\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn public_and_self_signed_tracks_never_share_a_directory() {
        let dir = std::env::temp_dir().join(format!("brocade-cert-tracks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut public = material("PUBLIC", "PUBLIC-KEY");
        public.track = CertificateTrack::PublicCa;
        write_certificate(&dir, &public).unwrap();
        write_certificate(&dir, &material("SELF", "SELF-KEY")).unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.join(CERT_DIR).join(PUBLIC_CA_DIR).join(SLOT_A_FILE))
                .unwrap(),
            "PUBLIC\nPUBLIC-KEY\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(CERT_DIR).join(SELF_SIGNED_DIR).join(SLOT_A_FILE))
                .unwrap(),
            "SELF\nSELF-KEY\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
