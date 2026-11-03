use super::{
    GitHubClient, GitHubError, summary,
    transport::{number, text},
};
use scion_core::{
    Outcome,
    observation::{CheckRun, CommitStatus, Observation, RequiredCheck, Review, Revision},
    validation::{valid_repository, valid_sha},
};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(PartialEq, Eq)]
struct Pr {
    head: String,
    merge: Option<String>,
    branch: String,
    state: String,
    draft: bool,
    author: u64,
}

fn pr(value: &Value, repository: &str, id: u64) -> Result<Pr, GitHubError> {
    if number(&value["number"])? != id
        || !text(&value["base"]["repo"]["full_name"])?.eq_ignore_ascii_case(repository)
    {
        return Err(GitHubError::InvalidResponse);
    }
    let head = text(&value["head"]["sha"])?;
    let merge = value["merge_commit_sha"].as_str().map(str::to_owned);
    if !valid_sha(&head) || merge.as_deref().is_some_and(|sha| !valid_sha(sha)) {
        return Err(GitHubError::InvalidResponse);
    }
    let state = if value["merged"]
        .as_bool()
        .ok_or(GitHubError::InvalidResponse)?
    {
        "merged".into()
    } else {
        text(&value["state"])?
    };
    if !["open", "closed", "merged"].contains(&state.as_str()) {
        return Err(GitHubError::InvalidResponse);
    }
    Ok(Pr {
        head,
        merge,
        branch: text(&value["base"]["ref"])?,
        state,
        draft: value["draft"]
            .as_bool()
            .ok_or(GitHubError::InvalidResponse)?,
        author: number(&value["user"]["id"])?,
    })
}

impl GitHubClient {
    /// Observe only upstream facts, rechecking head and the live base tip at the end of the poll.
    pub fn observe(&self, repository: &str, id: u64) -> Result<Observation, GitHubError> {
        if !valid_repository(repository) || id == 0 || id > i64::MAX as u64 {
            return Err(GitHubError::InvalidInput);
        }
        let (owner, name) = repository
            .split_once('/')
            .ok_or(GitHubError::InvalidInput)?;
        let id_text = id.to_string();
        let path = ["repos", owner, name, "pulls", &id_text];
        let initial = pr(&self.value(&path)?, repository, id)?;
        let base_path = ["repos", owner, name, "git", "ref", "heads", &initial.branch];
        let base = text(&self.value(&base_path)?["object"]["sha"])?;
        if !valid_sha(&base) {
            return Err(GitHubError::InvalidResponse);
        }
        let checks = self
            .pages(
                &["repos", owner, name, "commits", &initial.head, "check-runs"],
                Some("check_runs"),
            )?
            .iter()
            .map(check)
            .collect::<Result<Vec<_>, _>>()?;
        let commit_statuses = self
            .pages(
                &["repos", owner, name, "commits", &initial.head, "statuses"],
                None,
            )?
            .iter()
            .map(commit_status)
            .collect::<Result<Vec<_>, _>>()?;
        let reviews = self
            .pages(&["repos", owner, name, "pulls", &id_text, "reviews"], None)?
            .iter()
            .map(review)
            .collect::<Result<Vec<_>, _>>()?;
        let (required_checks, protection_known, mut issues) =
            self.requirements(owner, name, &initial.branch);
        let latest = pr(&self.value(&path)?, repository, id)?;
        let latest_base = text(&self.value(&base_path)?["object"]["sha"])?;
        let stable = initial == latest && base == latest_base;
        if !stable {
            issues.push("PR head, live base or PR state changed during observation".into());
        }
        let mut observation = Observation {
            schema_version: 1,
            revision: Revision {
                repository: repository.to_ascii_lowercase(),
                pull_request: id,
                head_sha: initial.head,
                base_sha: base,
                merge_sha: initial.merge,
            },
            state: initial.state,
            draft: initial.draft,
            author_id: initial.author,
            base_branch: initial.branch,
            stable,
            protection_known,
            required_checks,
            checks,
            commit_statuses,
            reviews,
            head_ci: Outcome::Blocked,
            review_policy: Outcome::NotRun,
            issues,
        };
        observation.checks.sort_by_key(|c| c.id);
        let mut ids = BTreeSet::new();
        if observation.checks.iter().any(|c| !ids.insert(c.id)) {
            return Err(GitHubError::InvalidResponse);
        }
        observation.commit_statuses.sort_by_key(|s| s.id);
        ids.clear();
        if observation
            .commit_statuses
            .iter()
            .any(|s| !ids.insert(s.id))
        {
            return Err(GitHubError::InvalidResponse);
        }
        summary::apply(&mut observation);
        Ok(observation)
    }

    fn requirements(
        &self,
        owner: &str,
        name: &str,
        branch: &str,
    ) -> (Vec<RequiredCheck>, bool, Vec<String>) {
        let mut required = BTreeSet::new();
        let mut issues = Vec::new();
        let mut known = true;
        match self.request(
            &[
                "repos",
                owner,
                name,
                "branches",
                branch,
                "protection",
                "required_status_checks",
            ],
            None,
        ) {
            Ok((200, value)) => match classic(&value) {
                Ok(checks) => required.extend(checks),
                Err(_) => {
                    known = false;
                    issues.push("invalid classic required checks response".into());
                }
            },
            Ok((404, value))
                if ["Branch not protected", "Required status checks not enabled"]
                    .contains(&value["message"].as_str().unwrap_or("")) => {}
            _ => {
                known = false;
                issues.push("classic required checks unavailable or permission unknown".into());
            }
        }
        match self.pages(&["repos", owner, name, "rules", "branches", branch], None) {
            Ok(rules) => {
                for rule in rules {
                    if rule["type"] == "required_status_checks" {
                        match ruleset(&rule["parameters"]["required_status_checks"]) {
                            Ok(checks) => required.extend(checks),
                            Err(_) => {
                                known = false;
                                issues.push("invalid ruleset required checks response".into());
                            }
                        }
                    }
                }
            }
            Err(_) => {
                known = false;
                issues.push("active branch rules unavailable".into());
            }
        }
        (required.into_iter().collect(), known, issues)
    }
}

fn classic(value: &Value) -> Result<Vec<RequiredCheck>, GitHubError> {
    let mut result = Vec::new();
    if let Some(checks) = value["checks"].as_array() {
        for check in checks {
            result.push(RequiredCheck {
                name: text(&check["context"])?,
                app_id: check["app_id"].as_u64().filter(|id| *id > 0),
            });
        }
    }
    for context in value["contexts"]
        .as_array()
        .ok_or(GitHubError::InvalidResponse)?
    {
        let name = text(context)?;
        if !result.iter().any(|c| c.name == name) {
            result.push(RequiredCheck { name, app_id: None });
        }
    }
    Ok(result)
}
fn ruleset(value: &Value) -> Result<Vec<RequiredCheck>, GitHubError> {
    value
        .as_array()
        .ok_or(GitHubError::InvalidResponse)?
        .iter()
        .map(|c| {
            Ok(RequiredCheck {
                name: text(&c["context"])?,
                app_id: c["integration_id"].as_u64().filter(|id| *id > 0),
            })
        })
        .collect()
}
fn check(value: &Value) -> Result<CheckRun, GitHubError> {
    Ok(CheckRun {
        id: number(&value["id"])?,
        name: text(&value["name"])?,
        app_id: number(&value["app"]["id"])?,
        head_sha: text(&value["head_sha"])?,
        status: text(&value["status"])?,
        conclusion: value["conclusion"].as_str().map(str::to_owned),
        url: text(&value["html_url"])?,
    })
}
fn review(value: &Value) -> Result<Review, GitHubError> {
    Ok(Review {
        id: number(&value["id"])?,
        author_id: if value["user"].is_null() {
            None
        } else {
            Some(number(&value["user"]["id"])?)
        },
        state: text(&value["state"])?,
        commit_sha: if value["commit_id"].is_null() {
            None
        } else {
            Some(text(&value["commit_id"])?)
        },
    })
}

fn commit_status(value: &Value) -> Result<CommitStatus, GitHubError> {
    Ok(CommitStatus {
        id: number(&value["id"])?,
        context: text(&value["context"])?,
        state: text(&value["state"])?,
    })
}
