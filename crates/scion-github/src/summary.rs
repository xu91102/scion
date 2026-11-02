use scion_core::{Outcome, observation::Observation};

pub(crate) fn apply(observation: &mut Observation) {
    observation.head_ci = ci(observation);
    observation.review_policy = Outcome::NotRun;
}

fn ci(observation: &mut Observation) -> Outcome {
    if !observation.stable || !observation.protection_known {
        return Outcome::Blocked;
    }
    if observation.required_checks.is_empty() {
        observation
            .issues
            .push("no required checks configured; green checks alone are insufficient".into());
        return Outcome::Blocked;
    }
    let mut blocked = false;
    let mut failed = false;
    for required in &observation.required_checks {
        if observation
            .commit_statuses
            .iter()
            .any(|s| s.context == required.name)
        {
            blocked = true;
            observation.issues.push(format!(
                "required context {} has legacy commit statuses that M1 does not verify",
                required.name
            ));
        }
        let Some(app) = required.app_id else {
            blocked = true;
            observation.issues.push(format!(
                "required check {} has no fixed source App",
                required.name
            ));
            continue;
        };
        let matching: Vec<_> = observation
            .checks
            .iter()
            .filter(|c| {
                c.name == required.name
                    && c.app_id == app
                    && c.head_sha == observation.revision.head_sha
            })
            .collect();
        if matching.len() != 1 {
            blocked = true;
            observation.issues.push(format!(
                "required check {} missing or ambiguous for its source App",
                required.name
            ));
            continue;
        }
        let check = matching[0];
        if check.status != "completed" {
            blocked = true;
            continue;
        }
        match check.conclusion.as_deref() {
            Some("success") => {}
            Some("failure" | "cancelled" | "timed_out" | "action_required" | "startup_failure") => {
                failed = true
            }
            _ => blocked = true,
        }
    }
    if failed {
        Outcome::Fail
    } else if blocked {
        Outcome::Blocked
    } else {
        Outcome::Pass
    }
}
