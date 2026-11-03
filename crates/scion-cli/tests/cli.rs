use std::{path::PathBuf, process::Command};

fn example(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
}

#[test]
fn ready_example_produces_an_advisory_json_report() {
    let output = Command::new(env!("CARGO_BIN_EXE_scion"))
        .arg("evaluate")
        .arg(example("ready.json"))
        .output()
        .expect("run CLI");
    assert!(output.status.success());
    assert!(
        output.stdout.starts_with(b"{"),
        "CLI must emit a JSON report"
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["decision"], "ready");
    assert_eq!(report["mode"], "dry_run");
    assert_eq!(report["evidence_verified"], false);
    assert_eq!(report["merge_performed"], false);
}

#[test]
fn blocked_example_uses_a_distinct_nonzero_exit_code() {
    let output = Command::new(env!("CARGO_BIN_EXE_scion"))
        .arg("evaluate")
        .arg(example("blocked.json"))
        .output()
        .expect("run CLI");
    assert_eq!(output.status.code(), Some(3));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["decision"], "blocked");
    assert_eq!(report["merge_performed"], false);
}

struct TestInput(PathBuf);

impl TestInput {
    fn new(bytes: &[u8]) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("scion-cli-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).expect("new private test directory");
        let path = dir.join("input.json");
        std::fs::write(&path, bytes).expect("write test input");
        Self(path)
    }
}

impl Drop for TestInput {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.parent().expect("test directory"))
            .expect("remove own test input");
    }
}

#[test]
fn invalid_json_unknown_fields_and_oversized_input_are_errors() {
    let mut unknown: serde_json::Value =
        serde_json::from_slice(&std::fs::read(example("ready.json")).expect("example"))
            .expect("example JSON");
    unknown["unexpected"] = true.into();
    for bytes in [
        b"not json".to_vec(),
        serde_json::to_vec(&unknown).expect("serialize"),
        vec![b' '; 1024 * 1024 + 1],
    ] {
        let input = TestInput::new(&bytes);
        let output = Command::new(env!("CARGO_BIN_EXE_scion"))
            .arg("evaluate")
            .arg(&input.0)
            .output()
            .expect("run CLI");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&output.stderr).expect("JSON error");
        assert!(report["error"].is_string());
        assert_eq!(report["evidence_verified"], false);
        assert_eq!(report["merge_performed"], false);
    }
}

#[test]
fn missing_file_or_arguments_do_not_report_ready() {
    for args in [
        vec![],
        vec!["merge"],
        vec!["evaluate", "/path-that-does-not-exist/scion.json"],
        vec!["evaluate", "extra", "argument"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_scion"))
            .args(args)
            .output()
            .expect("run CLI");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn persisted_observation_is_read_in_a_new_cli_process_without_claiming_freshness() {
    use scion_core::{
        Outcome,
        observation::{Observation, Revision},
    };
    let input = TestInput::new(&[]);
    let observation = Observation {
        schema_version: 1,
        revision: Revision {
            repository: "example/project".into(),
            pull_request: 42,
            head_sha: "a".repeat(40),
            base_sha: "b".repeat(40),
            merge_sha: None,
        },
        state: "open".into(),
        draft: false,
        author_id: 1,
        base_branch: "main".into(),
        stable: true,
        protection_known: false,
        required_checks: vec![],
        checks: vec![],
        reviews: vec![],
        commit_statuses: vec![],
        head_ci: Outcome::Blocked,
        review_policy: Outcome::NotRun,
        issues: vec![],
    };
    let mut store = scion_store::Store::open(&input.0).expect("open");
    store.record("one", &observation).expect("record");
    drop(store);
    let output = Command::new(env!("CARGO_BIN_EXE_scion"))
        .arg("history")
        .arg(&input.0)
        .args(["example/project", "42"])
        .output()
        .expect("run history");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("report");
    assert_eq!(report["mode"], "cached_history");
    assert_eq!(report["github_observed"], false);
    assert_eq!(report["evidence_verified"], false);
    assert_eq!(report["merge_performed"], false);
    assert_eq!(report["records"][0]["delivery_id"], "one");
}

#[test]
fn task_creation_and_new_process_readback_keep_the_task_queued() {
    let input = TestInput::new(&[]);
    let database = input.0.parent().expect("directory").join("tasks.sqlite");
    let output = Command::new(env!("CARGO_BIN_EXE_scion"))
        .arg("task-create")
        .arg(example("task.json"))
        .arg(&database)
        .output()
        .expect("create process");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_scion"))
        .arg("task-show")
        .arg(&database)
        .arg("csv-export")
        .output()
        .expect("new read process");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(value["task"]["state"], "queued");
    assert_eq!(value["task"]["spec"]["max_attempts"], 2);
    assert_eq!(value["merge_performed"], false);
    assert_eq!(value["github_observed"], false);
}

#[test]
fn task_errors_are_labeled_as_local_operations() {
    let output = Command::new(env!("CARGO_BIN_EXE_scion"))
        .arg("task-create")
        .output()
        .expect("error process");
    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(value["mode"], "local_task_error");
    assert_eq!(value["merge_performed"], false);
}
