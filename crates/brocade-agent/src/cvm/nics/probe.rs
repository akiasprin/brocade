//! Overlapping NIC addresses are probed inside their own consumer namespace.
use super::*;
use std::{fs::File, os::fd::AsRawFd};
pub(crate) fn service(
    interface: &str,
    host: &str,
    port: u16,
) -> Result<Option<(Reachability, Option<u32>)>> {
    let Some(root) = ROOT.get() else {
        return Ok(None);
    };
    let desired = snapshot(root)?;
    let Some(binding) = desired.bindings.iter().find(|b| {
        b.side == BindingSide::Consumer
            && b.interface_id == interface
            && b.phase == BindingPhase::Active
    }) else {
        return Ok(None);
    };
    let address: IpAddr = host
        .parse()
        .map_err(|_| "invalid allocated NIC probe address")?;
    if port == 0 || ![binding.nic.ipv4, binding.nic.ipv6].contains(&Some(address)) {
        return Err("service probe is outside its NIC allocation".into());
    }
    host::verify_anchor(root, binding)?;
    let path = Path::new("/run/netns").join(host::namespace(binding));
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|_| "NIC probe namespace is unavailable")?;
    if metadata.file_type().is_symlink() {
        return Err("NIC probe namespace ownership changed".into());
    }
    let namespace = File::open(path).map_err(|_| "cannot open owned NIC probe namespace")?;
    std::thread::spawn(move || {
        // SAFETY: this dedicated thread owns the checked namespace descriptor;
        // it has no async runtime and exits without affecting any other thread.
        if unsafe { libc::setns(namespace.as_raw_fd(), libc::CLONE_NEWNET) } < 0 {
            return Err("cannot enter allocated NIC probe namespace".into());
        }
        let started = std::time::Instant::now();
        Ok(Some(
            match std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::new(address, port),
                Duration::from_millis(250),
            ) {
                Ok(_) => (
                    Reachability::Reachable,
                    Some(started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32),
                ),
                Err(_) => (Reachability::Unreachable, None),
            },
        ))
    })
    .join()
    .map_err(|_| "allocated NIC probe worker stopped")?
}
