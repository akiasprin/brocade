//! Persistent Guest terminal transport, independent of desired-state convergence.
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "2";
pub const VERSION_HEADER: &str = "x-brocade-terminal-version";
pub const MAX_DATA: usize = 16 * 1024;
pub const MAX_CONTROL: usize = 2048;
pub use cube_terminal::{SessionInfo, HISTORY_BYTES};
pub const MAX_PER_NODE: usize = cube_terminal::MAX_SESSIONS;
/// A handover briefly needs both old and new transports for the same process.
pub const MAX_ATTACHMENTS_PER_NODE: usize = MAX_PER_NODE * 2;
pub const MAX_SESSIONS: usize = 128;
pub const MAX_SECONDS: u64 = 7200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub instance_id: String,
    pub runtime_id: String,
    pub restart_epoch: u64,
    pub reinstall_epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Offer {
    pub session_id: String,
    pub target: Target,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub new_session: bool,
    #[serde(default)]
    pub policy_revision: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Acknowledges bytes rendered by the browser, not merely received by a proxy.
    Ack {
        bytes: usize,
    },
    Close,
}

impl Input {
    pub fn valid(&self) -> bool {
        match self {
            Self::Resize { cols, rows } => (2..=500).contains(cols) && (1..=300).contains(rows),
            Self::Ack { bytes } => (1..=MAX_DATA).contains(bytes),
            Self::Close => true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Output {
    Ready,
    Session {
        id: String,
        sessions: Vec<SessionInfo>,
        cols: u16,
        rows: u16,
    },
    Replay {
        active: bool,
    },
    Exit {
        code: u32,
    },
    Error {
        message: String,
    },
}

pub fn valid_session(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_bounds_reject_invalid_frames() {
        assert!(!Input::Resize { cols: 0, rows: 24 }.valid());
        assert!(!Input::Ack {
            bytes: MAX_DATA + 1
        }
        .valid());
        assert!(Input::Resize { cols: 80, rows: 24 }.valid());
        assert!(!valid_session("../runtime"));
        assert!(valid_session(&"a".repeat(64)));
    }
}
