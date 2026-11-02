//! Offline JSON assessment. This executable has no network, process-runner or merge capability.

use scion_core::{Decision, DeliverySnapshot};
use std::{
    error::Error,
    fs::File,
    io::{Read, Write},
    process::ExitCode,
};

const MAX_INPUT_BYTES: u64 = 1024 * 1024;

fn run() -> Result<u8, Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
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

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"schema_version": 1, "mode": "dry_run", "evidence_verified": false, "merge_performed": false, "error": error.to_string()})
            );
            ExitCode::from(2)
        }
    }
}
