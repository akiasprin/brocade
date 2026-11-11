//! Supply NAT requirements without coupling ordinary Xray convergence to the Cube client.
use std::{collections::BTreeSet, net::IpAddr};

pub(crate) fn addresses() -> Result<BTreeSet<IpAddr>, String> {
    #[cfg(feature = "cvm")]
    return crate::cvm::nics::tracking_addresses();
    #[cfg(not(feature = "cvm"))]
    Ok(BTreeSet::new())
}

pub(crate) fn ports() -> Result<BTreeSet<(IpAddr, u16)>, String> {
    #[cfg(feature = "cvm")]
    return crate::cvm::nics::tracking_ports();
    #[cfg(not(feature = "cvm"))]
    Ok(BTreeSet::new())
}
