//! Experimental data contracts. Repository and commit identity travel with every evidence item.

use serde::{Deserialize, Serialize};

/// Exact code and policy revision to which a report applies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub repository: String,
    pub pull_request: u64,
    pub head_sha: String,
    pub base_sha: String,
    pub merge_sha: String,
    pub policy_sha: String,
}

/// Separate review, build/test and real user-flow acceptance results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    Review,
    Ci,
    Acceptance,
}

/// A missing or skipped result never counts as a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    Pass,
    Fail,
    Blocked,
    NotRun,
}

/// An evidence reference supplied by a caller; the offline CLI cannot authenticate its source.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub gate: Gate,
    pub outcome: Outcome,
    pub candidate: Candidate,
    pub producer: String,
    pub artifact: String,
}

/// Experimental policy. The future controller must load this from trusted configuration.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePolicy {
    #[serde(default)]
    pub auto_merge: bool,
    #[serde(default)]
    pub require_human_approval: bool,
}

/// Input to local delivery assessment, not a trusted merge request or approval receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverySnapshot {
    pub schema_version: u32,
    pub candidate: Candidate,
    pub coding_actor: String,
    #[serde(default)]
    pub policy: MergePolicy,
    #[serde(default)]
    pub human_approved: bool,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

/// Stable reason identifiers for the experimental CLI's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerCode {
    InvalidInput,
    MissingEvidence,
    DuplicateEvidence,
    StaleEvidence,
    NotPassed,
    MissingArtifact,
    SelfReview,
    AutomationDisabled,
    HumanApprovalRequired,
}

/// A failed requirement, retaining the gate it belongs to.
#[derive(Clone, Debug, Serialize)]
pub struct Blocker {
    pub code: BlockerCode,
    pub gate: Option<Gate>,
    pub message: String,
}

/// Readiness under supplied facts; it does not authorize a GitHub write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Ready,
    Blocked,
}

/// Explicitly advisory output: no evidence authentication or merge is performed.
#[derive(Clone, Debug, Serialize)]
pub struct Assessment {
    pub schema_version: u32,
    pub mode: &'static str,
    pub evidence_verified: bool,
    pub merge_performed: bool,
    pub decision: Decision,
    pub candidate: Candidate,
    pub blockers: Vec<Blocker>,
}
