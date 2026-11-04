//! Local IPv6 capability for machine-only public probes. Chains use exit evidence instead.
use std::{fs, io, net::Ipv6Addr};

pub(super) fn available() -> io::Result<bool> {
    from_inventory(
        &fs::read_to_string("/proc/net/if_inet6")?,
        &fs::read_to_string("/proc/net/ipv6_route")?,
    )
}

fn address(raw: &str) -> io::Result<Ipv6Addr> {
    let value = u128::from_str_radix(raw, 16)
        .ok()
        .filter(|_| raw.len() == 32)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid IPv6 inventory address")
        })?;
    Ok(value.into())
}

fn from_inventory(addresses: &str, routes: &str) -> io::Result<bool> {
    for line in addresses.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 6 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid IPv6 address inventory",
            ));
        }
        let ip = address(fields[0])?;
        if !ip.is_unspecified()
            && !ip.is_loopback()
            && !ip.is_multicast()
            && !ip.is_unicast_link_local()
            && !ip.is_unique_local()
            && ip.to_ipv4_mapped().is_none()
        {
            // A configured public source without a route is a failure, not family absence.
            return Ok(true);
        }
    }
    for line in routes.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 10 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid IPv6 route inventory",
            ));
        }
        let prefix = u8::from_str_radix(fields[1], 16)
            .ok()
            .filter(|prefix| *prefix <= 128)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid IPv6 route prefix")
            })?;
        let flags = u32::from_str_radix(fields[8], 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid IPv6 route flags"))?;
        let ip = address(fields[0])?;
        // The kernel's unreachable default is not UP. ULA/link-local/loopback/multicast
        // routes alone only support local traffic. Keep default and public prefix routes,
        // including ULA-based NAT66 and policy routes in other tables.
        if flags & 1 != 0
            && (prefix == 0
                || !((prefix >= 7 && ip.is_unique_local())
                    || (prefix >= 10 && ip.is_unicast_link_local())
                    || (prefix >= 8 && ip.is_multicast())
                    || (prefix == 128 && (ip.is_loopback() || ip.is_unspecified()))))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ip(value: &str) -> String {
        format!("{:032x}", u128::from(value.parse::<Ipv6Addr>().unwrap()))
    }
    fn source(value: &str) -> String {
        format!("{} 01 80 00 80 bh000000001\n", ip(value))
    }
    fn route(value: &str, prefix: u8, flags: u32) -> String {
        format!(
            "{} {prefix:02x} {} 00 {} 00000100 00000000 00000000 {flags:08x} bh000000001\n",
            ip(value),
            ip("::"),
            ip("::")
        )
    }
    #[test]
    fn internal_ipv6_without_a_public_route_is_not_internet_egress() {
        let sources = source("::1") + &source("fe80::1") + &source("fd00::1");
        let routes = route("::", 0, 0x200200)
            + &route("fd00::", 64, 1)
            + &route("fe80::", 64, 1)
            + &route("ff00::", 8, 1)
            + &route("::1", 128, 1);
        assert!(!from_inventory(&sources, &routes).unwrap());
        assert!(!from_inventory("", "").unwrap());
    }
    #[test]
    fn public_sources_and_ula_with_public_routes_keep_real_failures() {
        assert!(from_inventory(&source("2001:db8::1"), "").unwrap());
        for (network, prefix) in [("::", 0), ("::", 1), ("2606:4700::", 32)] {
            assert!(from_inventory(&source("fd00::1"), &route(network, prefix, 1)).unwrap());
        }
    }
    #[test]
    fn unreadable_or_malformed_inventory_never_proves_absence() {
        for (sources, routes) in [
            ("malformed", ""),
            ("", "invalid"),
            ("", "not-an-address 00 0 00 0 0 0 0 00000001 lo"),
        ] {
            assert!(from_inventory(sources, routes).is_err());
        }
    }
}
