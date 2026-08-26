//! Which agent build the fleet has been cleared to install.
//!
//! Alongside `distribution.rs` rather than `settings.rs`, for its reason: this compiles into no
//! artifact, so writing it stamps no revision and travels through no release. Releasing an agent
//! is not a change to the model — the model does not know agents exist.
//!
//! # What is stored, and what that buys
//!
//! A *build id*, not a version and not a flag: the sha256 of the control plane's two embedded
//! per-architecture agent sha256s, joined in order. The control plane can only ever serve the
//! agents it was compiled with, so it refuses to serve any id but its own. Two consequences, both
//! wanted:
//!
//! - Deploying a new control plane does not release its agent. The recorded id stops matching, and
//!   every node is told 204 until somebody releases on purpose. With a flag here instead, every
//!   control-plane deploy would replace the agent on every machine at once.
//! - "Roll the fleet back" is not expressible, and honestly so. Rolling back means serving bytes
//!   this control plane does not have; the way back is to deploy the previous control plane, which
//!   carries the previous agent. Pretending otherwise would need the control plane to store old
//!   binaries, which is a different feature.
//!
//! # Rolling the control plane back does not roll the agents back
//!
//! Worth stating on its own, because it is the first thing anybody asks and the answer is not the
//! intuitive one. The clearance lives in the database and the database does not roll back with the
//! binary. So after a rollback the recorded id names the *newer* build while the process can only
//! serve the older one, the comparison above fails, and every node gets 204 — the fleet stays on
//! the agent it already took. The console shows this rather than leaving it silent
//! (`available_release_id` next to the recorded one).
//!
//! Making the fleet follow the rollback is a second, deliberate act: release again, which now
//! clears the older build, and the agents install it. Nothing distinguishes that from an upgrade —
//! an agent compares its own sha against the one it is told and installs anything different, in
//! either direction. There is no notion of newer.
//!
//! The state in between deserves care, because self-update is what makes it easy to reach: a fleet
//! running agents *newer* than the control plane driving them. Field-level compatibility mostly
//! holds — the protocol structs default their unknown fields — but an agent that has learned to
//! ask for something the older control plane never served will keep asking. `/agent/v1/agent-release`
//! is itself the example: answered 404 there, which the agent treats as "no self-update here"
//! rather than as an error (`selfupdate.rs`).
//!
//! # Why there is no percentage
//!
//! `nodes` takes an explicit list. The value of staging a rollout here is that a person installs
//! it on one machine and *looks* — at whether it still reports in, at whether its convergence
//! still lands. A percentage is the shape that invites skipping the looking, and the failure it
//! would let through is the one that matters: an agent that runs but can no longer be reached.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::{AdminContext, Result, StoreError};

/// How far a release reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentReleaseScope {
    /// Nobody upgrades. The recorded build id is kept rather than cleared, so that pausing and
    /// resuming does not mean choosing the build again.
    Off,
    /// Only the nodes named in `nodes`.
    Nodes,
    /// The whole fleet.
    All,
}

impl AgentReleaseScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Nodes => "nodes",
            Self::All => "all",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "off" => Ok(Self::Off),
            "nodes" => Ok(Self::Nodes),
            "all" => Ok(Self::All),
            other => Err(StoreError::InvalidData(format!(
                "未知的 agent 发布范围 {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRelease {
    /// The build cleared for installation, or `None` where none ever was.
    pub release_id: Option<String>,
    pub scope: AgentReleaseScope,
    /// Meaningful only under `Nodes`. Kept as written under the other two scopes so that switching
    /// to `nodes` and back does not lose the list.
    pub nodes: Vec<String>,
    /// Why this was released. The only one of the fields below that a caller supplies; a build id
    /// says which bytes and never says what for.
    #[serde(default)]
    pub note: Option<String>,
    /// What the control plane could say about the build at the moment it was cleared. Filled in
    /// here, not by the caller — the values come from the running process's own compile-time
    /// constants, and letting a request set them would let it record a release that never happened.
    ///
    /// Snapshotted rather than read live: after a redeploy the process describes the build it now
    /// carries, while these describe the one that was actually cleared.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub released_at: Option<String>,
    #[serde(default)]
    pub released_by: Option<String>,
}

/// What the running control plane knows about the agents compiled into it, for the record it
/// writes when somebody releases.
///
/// Passed in rather than read here: these are compile-time constants of the console binary, and
/// store has no business knowing that crate exists.
#[derive(Debug, Clone, Copy)]
pub struct AgentBuildInfo<'a> {
    pub version: &'a str,
    pub commit: &'a str,
}

impl AgentRelease {
    fn reaches(&self, node_id: &str) -> bool {
        match self.scope {
            AgentReleaseScope::Off => false,
            AgentReleaseScope::All => true,
            AgentReleaseScope::Nodes => self.nodes.iter().any(|listed| listed == node_id),
        }
    }

    /// Whether one node should be offered `release_id`.
    ///
    /// Takes the control plane's own build id and compares it here rather than trusting the stored
    /// one: what is stored is a clearance, and a clearance for bytes this process does not have is
    /// not one it can act on. This is the check that makes redeploying the control plane safe.
    pub fn offers(&self, node_id: &str, embedded_release_id: &str) -> bool {
        if self.release_id.as_deref() != Some(embedded_release_id) {
            return false;
        }
        self.reaches(node_id)
    }

    /// Emergency compatibility offer: keep the configured rollout scope, but ignore the stored
    /// build id so a control-plane redeploy cannot strand an agent below the minimum protocol.
    pub fn offers_protocol_rescue(&self, node_id: &str) -> bool {
        self.release_id.is_some() && self.reaches(node_id)
    }
}

pub async fn load_agent_release(pool: &PgPool) -> Result<AgentRelease> {
    let row = sqlx::query(
        "SELECT agent_release_id, agent_release_scope, agent_release_nodes,
                agent_release_version, agent_release_commit, agent_release_note,
                agent_released_at::text AS agent_released_at, agent_released_by
           FROM control_state
          WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    let nodes: serde_json::Value = row.try_get("agent_release_nodes")?;
    Ok(AgentRelease {
        release_id: row.try_get("agent_release_id")?,
        scope: AgentReleaseScope::parse(row.try_get::<String, _>("agent_release_scope")?.as_str())?,
        nodes: serde_json::from_value(nodes).unwrap_or_default(),
        note: row.try_get("agent_release_note")?,
        version: row.try_get("agent_release_version")?,
        commit: row.try_get("agent_release_commit")?,
        released_at: row.try_get("agent_released_at")?,
        released_by: row.try_get("agent_released_by")?,
    })
}

/// Record a clearance.
///
/// `system-admin` only, matching `distribution.rs`. What this authorizes is every machine in the
/// fleet replacing the binary it runs as root, which is a strictly larger power than editing the
/// model — an operator who can publish a release can already reach every node's configuration, but
/// not the code that applies it.
pub async fn update_agent_release(
    pool: &PgPool,
    actor: &AdminContext,
    release: AgentRelease,
    build: AgentBuildInfo<'_>,
) -> Result<AgentRelease> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can release an agent".to_owned(),
        ));
    }
    let release = normalize(release);
    validate(&release)?;

    // The record is written from what the process knows and who is asking, never from the request
    // body: a caller able to set `released_by` could record somebody else's name against a
    // fleet-wide binary replacement, and one able to set the version could file a release under a
    // number the bytes have nothing to do with.
    let row = sqlx::query(
        "UPDATE control_state
            SET agent_release_id = $1, agent_release_scope = $2, agent_release_nodes = $3,
                agent_release_note = $4, agent_release_version = $5, agent_release_commit = $6,
                agent_released_at = now(), agent_released_by = $7
          WHERE id = TRUE
      RETURNING agent_released_at::text AS agent_released_at",
    )
    .bind(&release.release_id)
    .bind(release.scope.as_str())
    .bind(serde_json::to_value(&release.nodes).unwrap_or_else(|_| serde_json::json!([])))
    .bind(&release.note)
    .bind(build.version)
    .bind(build.commit)
    .bind(actor.operator_id())
    .fetch_one(pool)
    .await?;

    Ok(AgentRelease {
        version: Some(build.version.to_owned()),
        commit: Some(build.commit.to_owned()),
        released_at: row.try_get("agent_released_at")?,
        released_by: Some(actor.operator_id().to_owned()),
        ..release
    })
}

/// Trim, drop blanks, and de-duplicate the node list.
///
/// A form submits what somebody pasted, and a list with `""` in it would match no node while
/// looking like it names one — the operator would be watching a rollout that reaches one machine
/// fewer than the count says.
fn normalize(release: AgentRelease) -> AgentRelease {
    let mut nodes = Vec::new();
    for node in release.nodes {
        let node = node.trim().to_owned();
        if !node.is_empty() && !nodes.contains(&node) {
            nodes.push(node);
        }
    }
    AgentRelease {
        release_id: release
            .release_id
            .map(|id| id.trim().to_ascii_lowercase())
            .filter(|id| !id.is_empty()),
        scope: release.scope,
        nodes,
        note: release
            .note
            .map(|note| note.trim().to_owned())
            .filter(|note| !note.is_empty()),
        ..release_record_placeholders()
    }
}

/// The four fields the caller never supplies. `normalize` runs before they are known, and spelling
/// them as `None` inline would read as though a caller could have set them.
fn release_record_placeholders() -> AgentRelease {
    AgentRelease {
        release_id: None,
        scope: AgentReleaseScope::Off,
        nodes: Vec::new(),
        note: None,
        version: None,
        commit: None,
        released_at: None,
        released_by: None,
    }
}

fn validate(release: &AgentRelease) -> Result<()> {
    if let Some(id) = &release.release_id {
        if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(StoreError::InvalidData(
                "agent 构建号得是 64 位十六进制".to_owned(),
            ));
        }
    }
    // The same rule the database holds, checked here so the error says what is wrong rather than
    // arriving as a constraint violation.
    if release.scope != AgentReleaseScope::Off && release.release_id.is_none() {
        return Err(StoreError::InvalidData(
            "没有指定构建号就不能开启发布".to_owned(),
        ));
    }
    if release.scope == AgentReleaseScope::Nodes && release.nodes.is_empty() {
        return Err(StoreError::InvalidData(
            "范围是「指定节点」，但一个节点也没选".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD: &str = "aa11bb22cc33dd44ee55ff66aa77bb88cc99dd00ee11ff22aa33bb44cc55dd66";
    const OTHER: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn release(scope: AgentReleaseScope, nodes: &[&str]) -> AgentRelease {
        AgentRelease {
            release_id: Some(BUILD.to_owned()),
            scope,
            nodes: nodes.iter().map(|n| (*n).to_owned()).collect(),
            ..release_record_placeholders()
        }
    }

    #[test]
    fn off_offers_nobody() {
        assert!(!release(AgentReleaseScope::Off, &[]).offers("n1", BUILD));
    }

    #[test]
    fn all_offers_every_node() {
        let all = release(AgentReleaseScope::All, &[]);
        assert!(all.offers("n1", BUILD));
        assert!(all.offers("n2", BUILD));
    }

    #[test]
    fn nodes_offers_only_the_listed_ones() {
        let staged = release(AgentReleaseScope::Nodes, &["n1"]);
        assert!(staged.offers("n1", BUILD));
        assert!(!staged.offers("n2", BUILD));
    }

    /// The clearance names a build this control plane does not carry — which is what a redeployed
    /// control plane looks like. It must serve nothing, or deploying the control plane would
    /// double as releasing whatever agent it happens to embed.
    #[test]
    fn a_clearance_for_another_build_offers_nothing() {
        assert!(!release(AgentReleaseScope::All, &[]).offers("n1", OTHER));
    }

    #[test]
    fn never_released_offers_nothing() {
        let untouched = release_record_placeholders();
        assert!(!untouched.offers("n1", BUILD));
    }

    #[test]
    fn blank_and_duplicate_nodes_are_dropped() {
        let cleaned = normalize(AgentRelease {
            release_id: Some(format!("  {}  ", BUILD.to_uppercase())),
            scope: AgentReleaseScope::Nodes,
            nodes: vec![
                " n1 ".to_owned(),
                String::new(),
                "n1".to_owned(),
                "n2".to_owned(),
            ],
            ..release_record_placeholders()
        });
        assert_eq!(cleaned.nodes, vec!["n1".to_owned(), "n2".to_owned()]);
        // Upper case is folded rather than refused: the sha is displayed in several places and
        // somebody pasting it back with different case means the same build.
        assert_eq!(cleaned.release_id.as_deref(), Some(BUILD));
    }

    #[test]
    fn turning_it_on_without_a_build_is_refused() {
        let armed = AgentRelease {
            scope: AgentReleaseScope::All,
            ..release_record_placeholders()
        };
        assert!(validate(&armed).is_err());
    }

    /// Staging to an empty list would read as "released" in the UI while reaching nobody.
    #[test]
    fn staging_to_nobody_is_refused() {
        assert!(validate(&release(AgentReleaseScope::Nodes, &[])).is_err());
    }

    #[test]
    fn a_build_id_that_is_not_a_sha256_is_refused() {
        for bad in ["not-hex", "abc", &"f".repeat(63)] {
            let broken = AgentRelease {
                release_id: Some(bad.to_owned()),
                ..release_record_placeholders()
            };
            assert!(validate(&broken).is_err(), "{bad} 该被拒");
        }
    }

    /// A note is kept, trimmed, and a blank one becomes absent rather than an empty string.
    ///
    /// The distinction shows up on the page: `None` renders nothing, while `Some("")` renders a
    /// release that looks annotated and says nothing.
    #[test]
    fn a_blank_note_is_cleared_rather_than_stored() {
        let written = normalize(AgentRelease {
            note: Some("  先在新加坡验一轮  ".to_owned()),
            ..release_record_placeholders()
        });
        assert_eq!(written.note.as_deref(), Some("先在新加坡验一轮"));

        let blank = normalize(AgentRelease {
            note: Some("   ".to_owned()),
            ..release_record_placeholders()
        });
        assert_eq!(blank.note, None);
    }

    /// The four record fields are written from the process and the caller, never from the request.
    ///
    /// `normalize` runs before any of them is known, so it must drop whatever arrived — a request
    /// able to set `released_by` could file a fleet-wide binary replacement under somebody else's
    /// name, and one able to set `version` could record a release under a number its bytes have
    /// nothing to do with.
    #[test]
    fn the_record_fields_cannot_be_set_by_the_caller() {
        let claimed = normalize(AgentRelease {
            release_id: Some(BUILD.to_owned()),
            scope: AgentReleaseScope::All,
            version: Some("9.9.9".to_owned()),
            commit: Some("deadbee".to_owned()),
            released_at: Some("1999-01-01 00:00:00+00".to_owned()),
            released_by: Some("somebody-else".to_owned()),
            ..release_record_placeholders()
        });
        assert_eq!(claimed.version, None);
        assert_eq!(claimed.commit, None);
        assert_eq!(claimed.released_at, None);
        assert_eq!(claimed.released_by, None);
        // What the caller does own survives.
        assert_eq!(claimed.release_id.as_deref(), Some(BUILD));
        assert_eq!(claimed.scope, AgentReleaseScope::All);
    }

    /// Pausing keeps the build id, so resuming does not mean choosing it again.
    #[test]
    fn pausing_keeps_the_build() {
        let paused = normalize(AgentRelease {
            release_id: Some(BUILD.to_owned()),
            nodes: vec!["n1".to_owned()],
            ..release_record_placeholders()
        });
        assert!(validate(&paused).is_ok());
        assert_eq!(paused.release_id.as_deref(), Some(BUILD));
        assert_eq!(paused.nodes, vec!["n1".to_owned()]);
    }

    #[test]
    fn protocol_rescue_keeps_scope_but_ignores_a_stale_build_id() {
        let staged = AgentRelease {
            release_id: Some("0".repeat(64)),
            scope: AgentReleaseScope::Nodes,
            nodes: vec!["n1".to_owned()],
            ..release_record_placeholders()
        };
        assert!(!staged.offers("n1", BUILD));
        assert!(staged.offers_protocol_rescue("n1"));
        assert!(!staged.offers_protocol_rescue("n2"));

        let paused = AgentRelease {
            scope: AgentReleaseScope::Off,
            ..staged
        };
        assert!(!paused.offers_protocol_rescue("n1"));
    }
}
