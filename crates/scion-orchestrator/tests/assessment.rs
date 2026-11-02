use scion_core::{
    BlockerCode, Candidate, Decision, DeliverySnapshot, Evidence, Gate, MergePolicy, Outcome,
};
use scion_orchestrator::assess;

fn ready_snapshot() -> DeliverySnapshot {
    let candidate = Candidate {
        repository: "example/project".into(),
        pull_request: 42,
        head_sha: "a".repeat(40),
        base_sha: "b".repeat(40),
        merge_sha: "c".repeat(40),
        policy_sha: "d".repeat(40),
    };
    DeliverySnapshot {
        schema_version: 1,
        candidate: candidate.clone(),
        coding_actor: "coding-agent".into(),
        policy: MergePolicy {
            auto_merge: true,
            require_human_approval: false,
        },
        human_approved: false,
        evidence: [Gate::Review, Gate::Ci, Gate::Acceptance]
            .into_iter()
            .map(|gate| Evidence {
                gate,
                outcome: Outcome::Pass,
                candidate: candidate.clone(),
                producer: format!("{gate:?}-worker"),
                artifact: format!("https://example.invalid/evidence/{gate:?}"),
            })
            .collect(),
    }
}

#[test]
fn missing_required_evidence_blocks_delivery() {
    let mut snapshot = ready_snapshot();
    snapshot.evidence.clear();
    let report = assess(&snapshot);
    assert_eq!(report.decision, Decision::Blocked);
    assert_eq!(report.blockers.len(), 3);
    assert!(
        report
            .blockers
            .iter()
            .all(|b| b.code == BlockerCode::MissingEvidence)
    );
}

#[test]
fn evidence_for_another_candidate_is_rejected() {
    for field in 0..6 {
        let mut snapshot = ready_snapshot();
        let old = &mut snapshot.evidence[1].candidate;
        match field {
            0 => old.repository = "another/project".into(),
            1 => old.pull_request = 43,
            2 => old.head_sha = "e".repeat(40),
            3 => old.base_sha = "e".repeat(40),
            4 => old.merge_sha = "e".repeat(40),
            _ => old.policy_sha = "e".repeat(40),
        }
        assert!(
            assess(&snapshot)
                .blockers
                .iter()
                .any(|b| b.code == BlockerCode::StaleEvidence)
        );
    }
}

#[test]
fn every_gate_requires_a_pass() {
    for index in 0..3 {
        for outcome in [Outcome::Fail, Outcome::Blocked, Outcome::NotRun] {
            let mut snapshot = ready_snapshot();
            snapshot.evidence[index].outcome = outcome;
            assert!(
                assess(&snapshot)
                    .blockers
                    .iter()
                    .any(|b| b.code == BlockerCode::NotPassed)
            );
        }
    }
}

#[test]
fn acceptance_without_an_artifact_is_rejected() {
    let mut snapshot = ready_snapshot();
    snapshot.evidence[2].artifact = "  ".into();
    assert!(
        assess(&snapshot)
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::MissingArtifact)
    );
}

#[test]
fn duplicate_reports_are_ambiguous_even_when_both_pass() {
    let mut snapshot = ready_snapshot();
    snapshot.evidence.push(snapshot.evidence[1].clone());
    assert!(
        assess(&snapshot)
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::DuplicateEvidence)
    );
}

#[test]
fn coding_actor_cannot_supply_the_independent_review() {
    let mut snapshot = ready_snapshot();
    snapshot.evidence[0].producer = snapshot.coding_actor.clone();
    assert!(
        assess(&snapshot)
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::SelfReview)
    );
}

#[test]
fn absent_automation_configuration_defaults_to_disabled() {
    let mut snapshot = ready_snapshot();
    snapshot.policy = MergePolicy::default();
    assert!(
        assess(&snapshot)
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::AutomationDisabled)
    );
}

#[test]
fn required_human_approval_is_not_implied_by_other_checks() {
    let mut snapshot = ready_snapshot();
    snapshot.policy.require_human_approval = true;
    assert!(
        assess(&snapshot)
            .blockers
            .iter()
            .any(|b| b.code == BlockerCode::HumanApprovalRequired)
    );
    snapshot.human_approved = true;
    assert_eq!(assess(&snapshot).decision, Decision::Ready);
}

#[test]
fn malformed_candidate_or_evidence_identity_is_blocked() {
    let mut invalid = Vec::new();
    let mut snapshot = ready_snapshot();
    snapshot.schema_version = 2;
    invalid.push(snapshot);
    for repository in ["", "../project", "owner/project/extra", "owner/ space"] {
        let mut snapshot = ready_snapshot();
        snapshot.candidate.repository = repository.into();
        invalid.push(snapshot);
    }
    let mut snapshot = ready_snapshot();
    snapshot.candidate.pull_request = 0;
    invalid.push(snapshot);
    for sha in ["short".into(), "g".repeat(40), "a".repeat(41)] {
        for field in 0..4 {
            let mut snapshot = ready_snapshot();
            match field {
                0 => snapshot.candidate.head_sha = sha.clone(),
                1 => snapshot.candidate.base_sha = sha.clone(),
                2 => snapshot.candidate.merge_sha = sha.clone(),
                _ => snapshot.candidate.policy_sha = sha.clone(),
            }
            invalid.push(snapshot);
        }
    }
    let mut snapshot = ready_snapshot();
    snapshot.coding_actor.clear();
    invalid.push(snapshot);
    let mut snapshot = ready_snapshot();
    snapshot.evidence[0].producer = "  ".into();
    invalid.push(snapshot);
    for snapshot in invalid {
        assert!(
            assess(&snapshot)
                .blockers
                .iter()
                .any(|b| b.code == BlockerCode::InvalidInput)
        );
    }
}

#[test]
fn complete_facts_only_produce_an_unverified_dry_run_report() {
    let report = assess(&ready_snapshot());
    assert_eq!(report.decision, Decision::Ready);
    assert!(report.blockers.is_empty());
    assert_eq!(report.mode, "dry_run");
    assert!(!report.evidence_verified);
    assert!(!report.merge_performed);
}

#[test]
fn github_owner_grammar_is_checked_even_when_all_reports_agree() {
    for owner in [
        "-".into(),
        "-owner".into(),
        "owner-".into(),
        "owner.".into(),
        "owner_name".into(),
        "owner--name".into(),
        "o".repeat(40),
    ] {
        let mut snapshot = ready_snapshot();
        snapshot.candidate.repository = format!("{owner}/repo");
        for evidence in &mut snapshot.evidence {
            evidence.candidate = snapshot.candidate.clone();
        }
        assert_eq!(assess(&snapshot).decision, Decision::Blocked, "{owner}");
        assert!(
            assess(&snapshot)
                .blockers
                .iter()
                .any(|b| b.code == BlockerCode::InvalidInput)
        );
    }
    for owner in [
        "a".into(),
        "OpenAI".into(),
        "owner-name".into(),
        "o".repeat(39),
    ] {
        let mut snapshot = ready_snapshot();
        snapshot.candidate.repository = format!("{owner}/.github");
        for evidence in &mut snapshot.evidence {
            evidence.candidate = snapshot.candidate.clone();
        }
        assert_eq!(assess(&snapshot).decision, Decision::Ready, "{owner}");
    }
}

#[test]
fn github_repository_name_length_is_bounded() {
    for (length, decision) in [(100, Decision::Ready), (101, Decision::Blocked)] {
        let mut snapshot = ready_snapshot();
        snapshot.candidate.repository = format!("owner/{}", "r".repeat(length));
        for evidence in &mut snapshot.evidence {
            evidence.candidate = snapshot.candidate.clone();
        }
        assert_eq!(assess(&snapshot).decision, decision);
    }
}
