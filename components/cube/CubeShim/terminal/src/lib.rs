//! Private node-local protocol. CubeShim owns sessions; clients only attach.
use serde::{Deserialize, Serialize};

pub const ROOT: &str = "/run/brocade-cvm-terminals";
pub const VERSION: u32 = 1;
pub const MAX_SESSIONS: usize = 3;
pub const HISTORY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DATA: usize = 16 * 1024;
pub const MAX_FRAME: usize = 128 * 1024;

pub fn valid_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}
pub fn valid_runtime(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 150
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
pub fn socket_name(runtime: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}.sock", Sha256::digest(runtime.as_bytes()))
}
pub fn valid_size(cols: u16, rows: u16) -> bool {
    (2..=500).contains(&cols) && (1..=300).contains(&rows)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub owner: String,
    pub incarnation: String,
    #[serde(default)]
    pub policy_revision: u64,
    #[serde(flatten)]
    pub operation: Operation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Operation {
    Open {
        id: String,
        fresh: bool,
        exact: bool,
        cols: u16,
        rows: u16,
    },
    Attach {
        id: String,
        attachment: String,
    },
    Read {
        id: String,
        attachment: String,
    },
    Input {
        id: String,
        attachment: String,
        data: Vec<u8>,
    },
    Resize {
        id: String,
        attachment: String,
        cols: u16,
        rows: u16,
    },
    Detach {
        id: String,
        attachment: String,
    },
    Terminate {
        id: String,
        attachment: String,
    },
    Revoke {
        through_revision: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub running: bool,
    pub history_bytes: usize,
    pub history_truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Opened {
        id: String,
        sessions: Vec<SessionInfo>,
    },
    Attached {
        cols: u16,
        rows: u16,
    },
    Data {
        data: Vec<u8>,
        replay: bool,
        replay_done: bool,
        exit: Option<u32>,
    },
    Ok,
    Error {
        message: String,
    },
}

#[cfg(feature = "server")]
mod screen;
#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "server")]
pub mod process;
