use super::docker::capture;
use crate::Result;
use scion_core::{attempt::PullRequest, workspace::Workspace};
use serde_json::Value;
use std::process::Command;

pub fn reconcile(workspace: &Workspace, head: &str) -> Result<Option<PullRequest>> {
    let owner = workspace
        .repository
        .split_once('/')
        .ok_or("invalid repository")?
        .0;
    let endpoint = format!("repos/{}/pulls", workspace.repository);
    let mut command = Command::new("gh");
    command.args([
        "api",
        "--hostname",
        "github.com",
        "--method",
        "GET",
        &endpoint,
        "-f",
        "state=all",
        "-f",
        &format!("head={owner}:{}", workspace.branch),
        "-f",
        &format!("base={}", workspace.target),
        "-F",
        "per_page=100",
    ]);
    let rows: Vec<Value> = serde_json::from_str(&capture(command)?)?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => Ok(Some(receipt(row, workspace, head)?)),
        _ => Err("multiple PRs match the task branch".into()),
    }
}

pub fn create(workspace: &Workspace, head: &str) -> Result<PullRequest> {
    let mut command = Command::new("gh");
    command.args(["api", "--hostname", "github.com", "--method", "POST",
        &format!("repos/{}/pulls", workspace.repository), "-f", &format!("head={}", workspace.branch),
        "-f", &format!("base={}", workspace.target), "-f", "title=Scion Agent task",
        "-f", "body=Generated from a Scion task. Review the proposed changes and verify CI before merging.",
        "-F", "draft=true"]);
    let row = serde_json::from_str(&capture(command)?)?;
    receipt(&row, workspace, head)
}

fn receipt(row: &Value, workspace: &Workspace, head: &str) -> Result<PullRequest> {
    let same_repository = |side: &str| {
        row[side]["repo"]["full_name"]
            .as_str()
            .is_some_and(|repo| repo.eq_ignore_ascii_case(&workspace.repository))
    };
    if !same_repository("head")
        || !same_repository("base")
        || row["head"]["sha"] != head
        || row["head"]["ref"] != workspace.branch
        || row["base"]["ref"] != workspace.target
    {
        return Err("GitHub PR does not match the committed task".into());
    }
    let number = row["number"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("invalid PR number")?;
    if row["base"]["sha"] != workspace.base {
        return Err(
            format!("draft PR #{number} has a moved target; inspect it before continuing").into(),
        );
    }
    let state = row["state"]
        .as_str()
        .filter(|s| ["open", "closed"].contains(s))
        .ok_or("invalid PR state")?;
    let draft = row["draft"].as_bool().ok_or("invalid PR draft state")?;
    Ok(PullRequest {
        number,
        url: format!("https://github.com/{}/pull/{number}", workspace.repository),
        state: state.into(),
        draft,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pr_receipt_rejects_a_fork_or_an_unrelated_head() {
        let workspace = Workspace {
            task: "task".into(),
            repository: "example/scion".into(),
            target: "main".into(),
            base: "base".into(),
            branch: "scion/fixture".into(),
            path: "/tmp/fixture".into(),
            allowed: vec![],
            sparse: vec![],
            config_digest: String::new(),
            spec_digest: String::new(),
            phase: scion_core::workspace::Phase::Ready,
            revision: 2,
            last_error: None,
        };
        let mut row = json!({"head":{"repo":{"full_name":"example/scion"},"ref":"scion/fixture","sha":"fixed"},
            "base":{"repo":{"full_name":"example/scion"},"ref":"main","sha":"base"},"number":7,"state":"open","draft":true});
        assert_eq!(receipt(&row, &workspace, "fixed").unwrap().number, 7);
        assert!(receipt(&row, &workspace, "wrong").is_err());
        row["head"]["repo"]["full_name"] = json!("fork/scion");
        assert!(receipt(&row, &workspace, "fixed").is_err());
    }
}
