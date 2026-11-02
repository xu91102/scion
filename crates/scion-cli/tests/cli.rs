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
