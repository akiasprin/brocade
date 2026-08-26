//! Where nodes reach this control plane, and which xray they install.
//!
//! Separate from `settings.rs` for one reason, and it is not filing: a model setting compiles
//! into artifacts, so writing one stamps a revision and the change travels through a release.
//! These two compile into nothing. They decide what the enrolment command prints and what
//! `/enroll/dist` serves, and both are read at the moment somebody installs a machine. Sharing
//! `update_settings`'s path would put a domain typo in the same ledger as a real configuration
//! change, and carry it into `ModelSnapshot`, where recompiling an old revision would drag along
//! a domain no artifact has ever held.
//!
//! `None` means "not set here", which is not the same as an empty string: it falls through to the
//! environment variable the process started with, and then to the built-in default. That is what
//! lets a deployment already running on env vars upgrade without changing behaviour.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::{AdminContext, Result, StoreError};

/// Longest accepted value. Both fields end up in a shell command people paste, so the cap is
/// about keeping that command readable rather than about storage.
const MAX_LEN: usize = 255;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionSettings {
    /// The address nodes fetch the agent and `install.sh` from. Must be reachable *from a node*,
    /// which is why it cannot be derived from the listener: the control plane commonly binds
    /// loopback behind a reverse proxy, and that address means nothing on the other machine.
    pub agent_public_url: Option<String>,
    /// Which upstream xray release the fleet installs, as a tag (`v26.4.25`). Unset, the install
    /// script takes the newest — which is not safe here, argued on `AgentDistribution`.
    pub xray_version: Option<String>,
}

pub async fn load_distribution(pool: &PgPool) -> Result<DistributionSettings> {
    let row =
        sqlx::query("SELECT agent_public_url, xray_version FROM control_state WHERE id = TRUE")
            .fetch_one(pool)
            .await?;
    Ok(DistributionSettings {
        agent_public_url: row.try_get("agent_public_url")?,
        xray_version: row.try_get("xray_version")?,
    })
}

pub async fn update_distribution(
    pool: &PgPool,
    actor: &AdminContext,
    settings: DistributionSettings,
) -> Result<DistributionSettings> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update distribution settings".to_owned(),
        ));
    }
    let settings = normalize(settings);
    validate(&settings)?;

    // No revision, no release. Writing straight to the row is the whole point of this module
    // being separate — see the header.
    sqlx::query(
        "UPDATE control_state
            SET agent_public_url = $1, xray_version = $2
          WHERE id = TRUE",
    )
    .bind(&settings.agent_public_url)
    .bind(&settings.xray_version)
    .execute(pool)
    .await?;
    Ok(settings)
}

/// Blank means cleared, not stored.
///
/// A form submits `""` for a field somebody emptied, and keeping that would be a third state
/// between "set" and "unset" whose only behaviour is to override the env var with nothing —
/// the enrolment command would then name an empty host and fail at the first fetch.
fn normalize(settings: DistributionSettings) -> DistributionSettings {
    fn clean(value: Option<String>) -> Option<String> {
        value
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }
    DistributionSettings {
        agent_public_url: clean(settings.agent_public_url).map(|url| {
            // A trailing slash would produce `https://host//enroll/install.sh`. Most servers
            // forgive that; a reverse proxy matching on an exact prefix does not.
            url.trim_end_matches('/').to_owned()
        }),
        xray_version: clean(settings.xray_version),
    }
}

fn validate(settings: &DistributionSettings) -> Result<()> {
    if let Some(url) = &settings.agent_public_url {
        if url.len() > MAX_LEN {
            return Err(StoreError::InvalidData(format!(
                "Agent 请求地址最长 {MAX_LEN} 个字符"
            )));
        }
        // Scheme required rather than guessed. Defaulting to https would silently rewrite what
        // the operator typed, and defaulting to http would hand every node a plaintext download
        // of a binary it is about to run as root.
        if !url.starts_with("https://") && !url.starts_with("http://") {
            return Err(StoreError::InvalidData(
                "Agent 请求地址要带 http:// 或 https://".to_owned(),
            ));
        }
        // The value is pasted into a shell command, so anything that could end the argument has
        // to be refused here rather than escaped later.
        if url.contains(char::is_whitespace) || url.contains(['\'', '"', '`', '$', '\\']) {
            return Err(StoreError::InvalidData(
                "Agent 请求地址里不能有空白或引号".to_owned(),
            ));
        }
    }
    if let Some(version) = &settings.xray_version {
        if version.len() > MAX_LEN {
            return Err(StoreError::InvalidData(format!(
                "xray 版本最长 {MAX_LEN} 个字符"
            )));
        }
        // Same reason as above: this one reaches the shell too, as `--xray-version <tag>`.
        if !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | 'v'))
        {
            return Err(StoreError::InvalidData(
                "xray 版本只能用字母、数字和 . - _".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blank and whitespace clear the field rather than storing an override of nothing.
    #[test]
    fn blank_clears_rather_than_overriding_with_nothing() {
        let cleared = normalize(DistributionSettings {
            agent_public_url: Some("   ".to_owned()),
            xray_version: Some(String::new()),
        });
        assert_eq!(cleared, DistributionSettings::default());
    }

    #[test]
    fn a_trailing_slash_is_dropped() {
        let settings = normalize(DistributionSettings {
            agent_public_url: Some("https://a.example.net/".to_owned()),
            xray_version: None,
        });
        assert_eq!(
            settings.agent_public_url.as_deref(),
            Some("https://a.example.net")
        );
    }

    /// The scheme is required, not guessed — see the argument in `validate`.
    #[test]
    fn a_url_without_a_scheme_is_refused() {
        let settings = DistributionSettings {
            agent_public_url: Some("a.example.net".to_owned()),
            xray_version: None,
        };
        assert!(validate(&settings).is_err());
    }

    /// Both values are pasted into a shell command, so a quote is refused at the door.
    #[test]
    fn shell_metacharacters_are_refused() {
        for url in [
            "https://a.example.net'; rm -rf /",
            "https://a.example.net $(id)",
            "https://a b.example.net",
        ] {
            let settings = DistributionSettings {
                agent_public_url: Some(url.to_owned()),
                xray_version: None,
            };
            assert!(validate(&settings).is_err(), "{url} 该被拒");
        }
        let settings = DistributionSettings {
            agent_public_url: None,
            xray_version: Some("v26.4.25; id".to_owned()),
        };
        assert!(validate(&settings).is_err());
    }

    #[test]
    fn ordinary_values_pass() {
        let settings = normalize(DistributionSettings {
            agent_public_url: Some("https://a.example.net".to_owned()),
            xray_version: Some("v26.4.25".to_owned()),
        });
        assert!(validate(&settings).is_ok());
    }
}
