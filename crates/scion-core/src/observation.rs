//! Read-only GitHub facts. These are not authenticated end-to-end delivery receipts.

use crate::Outcome;
use serde::{Deserialize, Serialize};

/// The PR head and live target-branch tip seen during a poll.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub repository: String,
    pub pull_request: u64,
    pub head_sha: String,
    pub base_sha: String,
    pub merge_sha: Option<String>,
}

/// A check is identified by both its context name and source GitHub App.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredCheck {
    pub name: String,
    pub app_id: Option<u64>,
}

/// Minimal check metadata, excluding check output and arbitrary repository content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRun {
    pub id: u64,
    pub name: String,
    pub app_id: u64,
    pub head_sha: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: String,
}

/// Legacy status metadata. M1 detects required contexts but does not verify this source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitStatus {
    pub id: u64,
    pub context: String,
    pub state: String,
}

/// Minimal review metadata. The body is deliberately not retained.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub id: u64,
    pub author_id: Option<u64>,
    pub state: String,
    pub commit_sha: Option<String>,
}

/// A bounded, read-only observation of one PR and its live base branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub schema_version: u32,
    pub revision: Revision,
    pub state: String,
    pub draft: bool,
    pub author_id: u64,
    pub base_branch: String,
    pub stable: bool,
    pub protection_known: bool,
    pub required_checks: Vec<RequiredCheck>,
    pub checks: Vec<CheckRun>,
    pub commit_statuses: Vec<CommitStatus>,
    pub reviews: Vec<Review>,
    pub head_ci: Outcome,
    /// Review authorization/independence policy is not executed by the M1 observer.
    pub review_policy: Outcome,
    pub issues: Vec<String>,
}
