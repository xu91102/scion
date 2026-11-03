use scion_coordinator::Coordinator;
use scion_core::attempt::Phase;
use serde_json::json;
use std::{error::Error, ffi::OsString, path::Path};

pub fn command(s: &str) -> bool {
    matches!(
        s,
        "task-run" | "task-cancel" | "task-attempts" | "task-publish"
    )
}

pub fn run(name: &str, args: &[OsString]) -> Result<u8, Box<dyn Error>> {
    let [db, task, extra @ ..] = args else {
        return Err("expected <database> <task-id>".into());
    };
    let task = task.to_str().ok_or("task ID must be UTF-8")?;
    let mut coordinator = Coordinator::open(Path::new(db))?;
    let (code, result) = match (name, extra) {
        ("task-run", [image]) | ("task-run", [image, _]) => {
            let publish = match extra {
                [_] => false,
                [_, flag] if flag == "--publish" => true,
                _ => {
                    return Err(
                        "usage: scion task-run <database> <task-id> <image-id> [--publish]".into(),
                    );
                }
            };
            let image = image.to_str().ok_or("image ID must be UTF-8")?;
            let mut attempt = coordinator.run_agent(task, image)?;
            if publish && attempt.phase == Phase::Committed {
                attempt = coordinator.publish_agent(task, None)?;
            }
            (
                if attempt.phase == Phase::Committed {
                    0
                } else {
                    3
                },
                json!({"attempt":attempt}),
            )
        }
        ("task-cancel", []) => (0, json!({"attempt":coordinator.cancel_agent(task)?})),
        ("task-attempts", []) => (0, json!({"attempts":coordinator.agent_attempts(task)?})),
        ("task-publish", []) => (0, json!({"attempt":coordinator.publish_agent(task, None)?})),
        ("task-publish", [flag, head]) if flag == "--reviewed-head" => {
            let head = head.to_str().ok_or("reviewed head must be UTF-8")?;
            (
                0,
                json!({"attempt":coordinator.publish_agent(task, Some(head))?}),
            )
        }
        _ => return Err("invalid Agent command arguments".into()),
    };
    println!(
        "{}",
        json!({"schema_version":1,"mode":"agent_task","evidence_verified":false,
        "merge_performed":false,"result":result})
    );
    Ok(code)
}
