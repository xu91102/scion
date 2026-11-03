use scion_core::{
    Outcome,
    observation::{Observation, Revision},
};
use scion_store::Store;
use std::path::PathBuf;

static NEXT_DB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct TestDb(PathBuf);
impl TestDb {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let sequence = NEXT_DB.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "scion-m1-{}-{stamp}-{sequence}.sqlite",
            std::process::id()
        )))
    }
}
impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn observation() -> Observation {
    Observation {
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
        issues: vec!["no required checks".into()],
    }
}

#[test]
fn a_record_survives_reopening_the_database() {
    let db = TestDb::new();
    let expected = observation();
    let mut store = Store::open(&db.0).expect("open");
    let result = store.record("delivery-1", &expected).expect("record event");
    assert!(!result.replayed);
    drop(store);
    let reopened = Store::open(&db.0).expect("reopen");
    let history = reopened.history("example/project", 42).expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].observation, expected);
    assert!(history[0].current);
}

#[test]
fn replays_do_not_duplicate_events_or_roll_back_the_current_revision() {
    let db = TestDb::new();
    let mut store = Store::open(&db.0).expect("open");
    let first = observation();
    store.record("first", &first).expect("first");
    assert!(store.record("first", &first).expect("replay").replayed);
    let mut changed = first.clone();
    changed.revision.head_sha = "c".repeat(40);
    store.record("second", &changed).expect("new head");
    assert!(
        !store
            .record("first", &first)
            .expect("old replay")
            .record
            .current
    );
    let history = store.history("example/project", 42).expect("history");
    assert_eq!(history.len(), 2);
    assert!(!history[0].current);
    assert!(history[1].current);
    assert_eq!(history[1].observation, changed);
}

#[test]
fn conflicting_delivery_payload_is_rejected_without_changing_the_task() {
    let db = TestDb::new();
    let mut store = Store::open(&db.0).expect("open");
    let first = observation();
    store.record("one", &first).expect("first");
    let mut changed = first.clone();
    changed.revision.base_sha = "c".repeat(40);
    assert!(matches!(
        store.record("one", &changed),
        Err(scion_store::StoreError::DeliveryConflict)
    ));
    let history = store.history("example/project", 42).expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].observation, first);
}

#[test]
fn foreign_or_newer_databases_are_not_initialized_over() {
    let db = TestDb::new();
    let conn = rusqlite::Connection::open(&db.0).expect("sqlite");
    conn.execute_batch(
        "CREATE TABLE user_data(value TEXT); INSERT INTO user_data VALUES ('preserve');",
    )
    .expect("foreign data");
    drop(conn);
    assert!(matches!(
        Store::open(&db.0),
        Err(scion_store::StoreError::UnsupportedDatabase)
    ));
    let own = TestDb::new();
    drop(Store::open(&own.0).expect("own"));
    let conn = rusqlite::Connection::open(&own.0).expect("sqlite");
    conn.pragma_update(None, "user_version", 3)
        .expect("future version");
    drop(conn);
    assert!(matches!(
        Store::open(&own.0),
        Err(scion_store::StoreError::UnsupportedDatabase)
    ));
}

#[test]
fn failed_task_update_rolls_back_the_event_insertion() {
    let db = TestDb::new();
    let mut store = Store::open(&db.0).expect("open");
    let conn = rusqlite::Connection::open(&db.0).expect("sqlite");
    conn.execute_batch("CREATE TRIGGER fail_task BEFORE INSERT ON tasks BEGIN SELECT RAISE(ABORT,'injected task write failure'); END;").expect("fault injection");
    assert!(store.record("retry-id", &observation()).is_err());
    conn.execute_batch("DROP TRIGGER fail_task;")
        .expect("remove fault");
    let mut changed = observation();
    changed.revision.head_sha = "c".repeat(40);
    assert!(
        !store
            .record("retry-id", &changed)
            .expect("must not conflict with rolled-back event")
            .replayed
    );
    assert_eq!(
        store.history("example/project", 42).expect("history").len(),
        1
    );
}

#[test]
fn whitespace_only_delivery_id_is_invalid() {
    let db = TestDb::new();
    let mut store = Store::open(&db.0).expect("open");
    assert!(matches!(
        store.record("   ", &observation()),
        Err(scion_store::StoreError::InvalidInput)
    ));
}

#[test]
fn queued_task_specification_survives_restart_without_executing() {
    let db = TestDb::new();
    let mut store = Store::open(&db.0).expect("open");
    let spec = scion_core::task::TaskSpec {
        id: "csv-export".into(),
        repository: "example/project".into(),
        target_branch: "main".into(),
        goal: "Add CSV export".into(),
        allowed_paths: vec!["src".into(), "tests".into()],
        acceptance_criteria: vec!["download has expected header".into()],
        owner: "operator".into(),
        max_attempts: 2,
        timeout_seconds: 300,
    };
    let task = store.create_task(&spec).expect("create task");
    assert_eq!(task.state, "queued");
    drop(store);
    let reopened = Store::open(&db.0).expect("reopen");
    assert_eq!(
        reopened
            .task("csv-export")
            .expect("read")
            .expect("exists")
            .spec,
        spec
    );
}

#[cfg(unix)]
#[test]
fn newly_created_database_is_private_to_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let db = TestDb::new();
    drop(Store::open(&db.0).expect("open"));
    assert_eq!(
        std::fs::metadata(&db.0)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o077,
        0
    );
}

#[test]
fn malformed_revision_is_rejected_before_persistence() {
    let db = TestDb::new();
    let mut store = Store::open(&db.0).expect("open");
    for field in 0..3 {
        let mut value = observation();
        match field {
            0 => value.revision.repository = "../project".into(),
            1 => value.revision.head_sha = "short".into(),
            _ => value.revision.base_sha = "z".repeat(40),
        }
        assert!(matches!(
            store.record("bad", &value),
            Err(scion_store::StoreError::InvalidInput)
        ));
    }
    assert!(
        store
            .history("example/project", 42)
            .expect("history")
            .is_empty()
    );
}

#[test]
fn task_scopes_cannot_name_git_metadata_or_escape_the_repository() {
    for path in ["../outside", "/tmp", ".git", "src/.GiT", "C:\\outside"] {
        let db = TestDb::new();
        let mut store = Store::open(&db.0).expect("open");
        let mut spec: scion_core::task::TaskSpec =
            serde_json::from_str(include_str!("../../../examples/task.json")).expect("spec");
        spec.allowed_paths = vec![path.into()];
        assert!(
            matches!(
                store.create_task(&spec),
                Err(scion_store::StoreError::InvalidInput)
            ),
            "{path}"
        );
    }
}

#[test]
fn task_ids_cannot_silently_widen_the_original_specification() {
    let db = TestDb::new();
    let mut store = Store::open(&db.0).expect("open");
    let spec: scion_core::task::TaskSpec =
        serde_json::from_str(include_str!("../../../examples/task.json")).expect("spec");
    let first = store.create_task(&spec).expect("first");
    let replay = store.create_task(&spec).expect("repeat");
    assert_eq!(first.created_at, replay.created_at);
    let mut changed = spec.clone();
    changed.allowed_paths.push("other".into());
    assert!(matches!(
        store.create_task(&changed),
        Err(scion_store::StoreError::TaskConflict)
    ));
    assert_eq!(
        store.task(&spec.id).expect("read").expect("task").spec,
        spec
    );
}
