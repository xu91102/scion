use super::MAX_INPUT_BYTES;
use scion_coordinator::Coordinator;
use std::{error::Error, ffi::OsString, fs::File, io::Read, path::Path};

pub fn run(command: &str, args: &[OsString]) -> Result<u8, Box<dyn Error>> {
    use serde_json::json;
    fn utf8(arg: &OsString) -> Result<&str, Box<dyn Error>> {
        arg.to_str().ok_or_else(|| "argument must be UTF-8".into())
    }
    let result = match (command, args) {
        ("state-upgrade", [db]) => json!({"backup":scion_store::Store::upgrade(Path::new(db))?}),
        ("project-register", [spec, db]) => {
            let mut data = Vec::new();
            File::open(spec)?
                .take(MAX_INPUT_BYTES + 1)
                .read_to_end(&mut data)?;
            if data.len() as u64 > MAX_INPUT_BYTES {
                return Err("project specification exceeds input limit".into());
            }
            let project = serde_json::from_slice(&data)?;
            json!({"project":Coordinator::open(Path::new(db))?.register(project)?})
        }
        ("workspace-prepare", [db, task, extra @ ..]) => {
            let sparse =
                match extra {
                    [] => vec![],
                    [flag, paths @ ..] if flag == "--sparse" && !paths.is_empty() => paths
                        .iter()
                        .map(|p| utf8(p).map(str::to_owned))
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => return Err(
                        "usage: scion workspace-prepare <database> <task-id> [--sparse <dir>...]"
                            .into(),
                    ),
                };
            match Coordinator::open(Path::new(db))?.prepare(utf8(task)?, sparse) {
                Ok(workspace) => json!({"workspace":workspace}),
                Err(error) => match error.downcast_ref::<scion_store::StoreError>() {
                    Some(scion_store::StoreError::PathOverlap {
                        task,
                        path,
                        occupied_path,
                    }) => {
                        json!({"outcome":"BLOCKED","occupying_task":task,"path":path,"occupied_path":occupied_path})
                    }
                    _ => return Err(error),
                },
            }
        }
        ("workspace-show", [db, task]) => Coordinator::open(Path::new(db))?.show(utf8(task)?)?,
        ("workspace-recover", [db]) => {
            let results = Coordinator::open(Path::new(db))?.recover()?;
            let blocked = results.iter().any(|r| r["outcome"] == "BLOCKED");
            json!({"outcome":if blocked {"BLOCKED"} else {"OK"},"results":results})
        }
        ("workspace-release", [db, task, flag]) if flag == "--confirm-stopped" => {
            json!({"workspace":Coordinator::open(Path::new(db))?.release(utf8(task)?)?})
        }
        ("workspace-preflight", [db, tasks @ ..]) if !tasks.is_empty() => {
            let tasks = tasks
                .iter()
                .map(|t| utf8(t).map(str::to_owned))
                .collect::<Result<Vec<_>, _>>()?;
            Coordinator::open(Path::new(db))?.preflight(&tasks)?
        }
        _ => return Err("invalid workspace command arguments; see docs/m2-workspaces.md".into()),
    };
    let code = if result
        .get("outcome")
        .is_some_and(|s| s != "CLEAN" && s != "OK")
    {
        3
    } else {
        0
    };
    println!(
        "{}",
        json!({"schema_version":1,"mode":"local_coordination","merge_performed":false,"evidence_verified":false,"result":result})
    );
    Ok(code)
}
