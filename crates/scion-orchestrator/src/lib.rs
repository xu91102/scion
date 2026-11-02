//! Pure, offline delivery assessment over the shared contracts.

use scion_core::{Assessment, Blocker, BlockerCode, Decision, DeliverySnapshot, Gate, Outcome};

/// Assess supplied facts without contacting GitHub or executing commands.
pub fn assess(snapshot: &DeliverySnapshot) -> Assessment {
    let mut blockers = Vec::new();
    if snapshot.schema_version != 1
        || !valid_candidate(&snapshot.candidate)
        || !valid_actor(&snapshot.coding_actor)
        || snapshot
            .evidence
            .iter()
            .any(|item| !valid_actor(&item.producer))
    {
        blockers.push(Blocker { code: BlockerCode::InvalidInput, gate: None,
            message: "expected schema 1, owner/repository, nonzero PR, full lowercase SHAs and nonempty actor identities".into() });
    }
    if !snapshot.policy.auto_merge {
        blockers.push(Blocker {
            code: BlockerCode::AutomationDisabled,
            gate: None,
            message: "automatic merge is disabled by the supplied policy".into(),
        });
    }
    if snapshot.policy.require_human_approval && !snapshot.human_approved {
        blockers.push(Blocker {
            code: BlockerCode::HumanApprovalRequired,
            gate: None,
            message: "the supplied policy requires human approval for this candidate".into(),
        });
    }
    check_evidence(snapshot, &mut blockers);
    Assessment {
        schema_version: 1,
        mode: "dry_run",
        evidence_verified: false,
        merge_performed: false,
        decision: if blockers.is_empty() {
            Decision::Ready
        } else {
            Decision::Blocked
        },
        candidate: snapshot.candidate.clone(),
        blockers,
    }
}

fn check_evidence(snapshot: &DeliverySnapshot, blockers: &mut Vec<Blocker>) {
    for gate in [Gate::Review, Gate::Ci, Gate::Acceptance] {
        let count = snapshot
            .evidence
            .iter()
            .filter(|item| item.gate == gate)
            .count();
        if count == 0 {
            blockers.push(Blocker {
                code: BlockerCode::MissingEvidence,
                gate: Some(gate),
                message: format!("missing {gate:?} evidence"),
            });
        }
        if count > 1 {
            blockers.push(Blocker {
                code: BlockerCode::DuplicateEvidence,
                gate: Some(gate),
                message: format!("expected one {gate:?} report, received {count}"),
            });
        }
    }
    for item in &snapshot.evidence {
        if item.gate == Gate::Review && item.producer == snapshot.coding_actor {
            blockers.push(Blocker {
                code: BlockerCode::SelfReview,
                gate: Some(Gate::Review),
                message: "review producer must differ from the coding actor".into(),
            });
        }
        if item.artifact.trim().is_empty() {
            blockers.push(Blocker {
                code: BlockerCode::MissingArtifact,
                gate: Some(item.gate),
                message: "evidence needs an inspectable artifact reference".into(),
            });
        }
        if item.outcome != Outcome::Pass {
            blockers.push(Blocker {
                code: BlockerCode::NotPassed,
                gate: Some(item.gate),
                message: format!("{:?} outcome is {:?}", item.gate, item.outcome),
            });
        }
        if item.candidate != snapshot.candidate {
            blockers.push(Blocker {
                code: BlockerCode::StaleEvidence,
                gate: Some(item.gate),
                message: "evidence is bound to another code or policy revision".into(),
            });
        }
    }
}

fn valid_candidate(candidate: &scion_core::Candidate) -> bool {
    valid_repository(&candidate.repository)
        && candidate.pull_request > 0
        && [
            &candidate.head_sha,
            &candidate.base_sha,
            &candidate.merge_sha,
            &candidate.policy_sha,
        ]
        .iter()
        .all(|sha| {
            matches!(sha.len(), 40 | 64)
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn valid_repository(repository: &str) -> bool {
    let Some((owner, name)) = repository.split_once('/') else {
        return false;
    };
    let valid_owner = !owner.is_empty()
        && owner.len() <= 39
        && owner
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && owner
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && !owner.contains("--")
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    valid_owner
        && !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn valid_actor(actor: &str) -> bool {
    !actor.is_empty()
        && actor.len() <= 256
        && !actor.chars().any(|c| c.is_whitespace() || c.is_control())
}
