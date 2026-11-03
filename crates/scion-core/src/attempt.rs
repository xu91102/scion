//! Persistent Agent execution and publication receipts.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Running,
    CancelRequested,
    Cancelled,
    TimedOut,
    Failed,
    Committed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub task: String,
    pub number: u32,
    pub key: String,
    pub base: String,
    pub image: String,
    #[serde(default = "credential_default")]
    pub credential_mounted: bool,
    pub daemon: String,
    pub phase: Phase,
    pub elapsed_ms: u64,
    pub exit_code: Option<i64>,
    pub head: Option<String>,
    pub changed_paths: Vec<String>,
    pub reason: Option<String>,
    pub pr: Option<PullRequest>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub state: String,
    pub draft: bool,
}

fn credential_default() -> bool {
    true
}
