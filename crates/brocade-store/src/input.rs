//! Validation and normalization of the values the console hands the store.
//!
//! These ran as private copies in `console`, `provision` and `admin`. The copies were identical,
//! which is what made them worth collapsing: they decide what an operator is allowed to store, and
//! two paths writing the same table under different rules is a field that passes through one door
//! and is refused at the next, for reasons no error message explains.

use std::net::Ipv6Addr;

use brocade_core::model::Dns;

use crate::{Result, StoreError};

/// A trimmed, non-empty copy of `value`.
///
/// Takes anything string-shaped so callers holding an owned field and callers holding a borrow
/// share one rule rather than one each.
pub(crate) fn required_text(value: impl AsRef<str>, field: &str) -> Result<String> {
    let value = value.as_ref().trim();
    if value.is_empty() {
        Err(StoreError::InvalidData(format!(
            "{field} must not be empty"
        )))
    } else {
        Ok(value.to_owned())
    }
}

/// An IPv6 literal with any surrounding brackets taken off, or `None` for blank.
///
/// Parsed and then thrown away: the parse is the check, the stored form is the text.
pub(crate) fn optional_ipv6_text(value: Option<&str>, field: &str) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let value = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(value);
    value
        .parse::<Ipv6Addr>()
        .map_err(|_| StoreError::InvalidData(format!("{field} must be an IPv6 address")))?;
    Ok(Some(value.to_owned()))
}

/// Port 0 is the kernel asking for any free port, which is never what an operator meant to type.
pub(crate) fn ensure_nonzero_port(value: u16, field: &str) -> Result<()> {
    if value == 0 {
        Err(StoreError::InvalidData(format!(
            "{field} must be between 1 and 65535"
        )))
    } else {
        Ok(())
    }
}

/// A servers list that is empty, or holds a blank entry, is a resolver setting that resolves
/// nothing — rejected here rather than shipped to a node that would fail to start on it.
pub(crate) fn validate_dns(dns: &Dns) -> Result<()> {
    match dns {
        Dns::System => Ok(()),
        Dns::Servers(servers) => {
            if servers.is_empty() {
                return Err(StoreError::InvalidData(
                    "dns servers must not be empty".to_owned(),
                ));
            }
            if servers.iter().any(|server| server.trim().is_empty()) {
                return Err(StoreError::InvalidData(
                    "dns servers must not contain empty values".to_owned(),
                ));
            }
            Ok(())
        }
    }
}

/// The `dns_kind` column's spelling.
pub(crate) fn dns_kind(dns: &Dns) -> &'static str {
    match dns {
        Dns::System => "system",
        Dns::Servers(_) => "servers",
    }
}
