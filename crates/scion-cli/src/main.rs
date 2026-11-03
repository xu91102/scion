//! Offline assessment and read-only GitHub observation with local persistence.

use scion_core::{Decision, DeliverySnapshot};
use std::{
    error::Error,
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    path::Path,
    process::ExitCode,
};

mod agent;
mod workspaces;

const MAX_INPUT_BYTES: u64 = 1024 * 1024;

fn run() -> Result<u8, Box<dyn Error>> {
    let all: Vec<OsString> = std::env::args_os().skip(1).collect();
    match all.first().and_then(|s| s.to_str()) {
        Some("observe") => return observe(&all[1..]),
        Some("history") => return history(&all[1..]),
        Some("task-create") => return task_create(&all[1..]),
        Some("task-show") => return task_show(&all[1..]),
        Some(command) if workspace_command(command) => return workspaces::run(command, &all[1..]),
        Some(command) if agent::command(command) => return agent::run(command, &all[1..]),
        _ => {}
    }
    let mut args = all.into_iter();
    let (Some(command), Some(path), None) = (args.next(), args.next(), args.next()) else {
        return Err("usage: scion evaluate <snapshot.json>".into());
    };
    if command != "evaluate" {
        return Err("usage: scion evaluate <snapshot.json>".into());
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err("snapshot exceeds the 1 MiB input limit".into());
    }
    let snapshot: DeliverySnapshot = serde_json::from_slice(&bytes)?;
    let report = scion_orchestrator::assess(&snapshot);
    let code = if report.decision == Decision::Ready {
        0
    } else {
        3
    };
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string(&report)?
    )?;
    Ok(code)
}

fn target(repository: &OsString, number: &OsString) -> Result<(String, u64), Box<dyn Error>> {
    let repository = repository.to_str().ok_or("repository must be UTF-8")?;
    let number: u64 = number.to_str().ok_or("PR number must be UTF-8")?.parse()?;
    if !scion_core::validation::valid_repository(repository)
        || number == 0
        || number > i64::MAX as u64
    {
        return Err("invalid GitHub repository or PR number".into());
    }
    Ok((repository.to_ascii_lowercase(), number))
}

fn observe(args: &[OsString]) -> Result<u8, Box<dyn Error>> {
    let [repository, number, database, delivery] = args else {
        return Err("usage: scion observe <owner/repo> <pr> <database> <delivery-id>".into());
    };
    let (repository, number) = target(repository, number)?;
    let delivery = delivery.to_str().ok_or("delivery ID must be UTF-8")?;
    if delivery.trim().is_empty() || delivery.len() > 256 || delivery.chars().any(char::is_control)
    {
        return Err("invalid delivery ID".into());
    }
    let token = match std::env::var("SCION_GITHUB_TOKEN") {
        Ok(token) => Some(token),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("SCION_GITHUB_TOKEN must be UTF-8".into()),
    };
    let observation =
        scion_github::GitHubClient::new(token.as_deref())?.observe(&repository, number)?;
    let result = scion_store::Store::open(Path::new(database))?.record(delivery, &observation)?;
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::json!({"schema_version":1,"mode":"github_read_only","github_observed":true,"evidence_verified":false,"merge_performed":false,"replayed":result.replayed,"record":result.record})
    )?;
    Ok(0)
}

fn task_create(args: &[OsString]) -> Result<u8, Box<dyn Error>> {
    let [spec_file, database] = args else {
        return Err("usage: scion task-create <task.json> <database>".into());
    };
    let mut bytes = Vec::new();
    File::open(spec_file)?
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err("task specification exceeds 1 MiB".into());
    }
    let spec: scion_core::task::TaskSpec = serde_json::from_slice(&bytes)?;
    if !spec.valid() {
        return Err("invalid task specification".into());
    }
    let task = scion_store::Store::open(Path::new(database))?.create_task(&spec)?;
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::json!({"schema_version":1,"mode":"local_task","github_observed":false,"evidence_verified":false,"merge_performed":false,"task":task})
    )?;
    Ok(0)
}

fn task_show(args: &[OsString]) -> Result<u8, Box<dyn Error>> {
    let [database, id] = args else {
        return Err("usage: scion task-show <database> <task-id>".into());
    };
    if !Path::new(database).is_file() {
        return Err("task database does not exist".into());
    }
    let id = id.to_str().ok_or("task ID must be UTF-8")?;
    let task = scion_store::Store::open(Path::new(database))?
        .task(id)?
        .ok_or("task not found")?;
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::json!({"schema_version":1,"mode":"local_task","github_observed":false,"evidence_verified":false,"merge_performed":false,"task":task})
    )?;
    Ok(0)
}

fn history(args: &[OsString]) -> Result<u8, Box<dyn Error>> {
    let [database, repository, number] = args else {
        return Err("usage: scion history <database> <owner/repo> <pr>".into());
    };
    let (repository, number) = target(repository, number)?;
    if !Path::new(database).is_file() {
        return Err("history database does not exist".into());
    }
    let records = scion_store::Store::open(Path::new(database))?.history(&repository, number)?;
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::json!({"schema_version":1,"mode":"cached_history","github_observed":false,"evidence_verified":false,"merge_performed":false,"records":records})
    )?;
    Ok(0)
}

fn workspace_command(command: &str) -> bool {
    matches!(
        command,
        "project-register"
            | "state-upgrade"
            | "workspace-prepare"
            | "workspace-show"
            | "workspace-preflight"
            | "workspace-release"
            | "workspace-recover"
    )
}

fn main() -> ExitCode {
    let command = std::env::args_os().nth(1);
    let mode = match command.as_deref().and_then(|s| s.to_str()) {
        Some("observe" | "history") => "read_only_error",
        Some("task-create" | "task-show") => "local_task_error",
        Some(command) if workspace_command(command) => "workspace_error",
        Some(command) if agent::command(command) => "agent_error",
        _ => "dry_run",
    };
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"schema_version": 1, "mode": mode, "evidence_verified": false, "merge_performed": false, "error": error.to_string()})
            );
            ExitCode::from(2)
        }
    }
}
