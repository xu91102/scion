//! Persistent observation history. SQLite transactions own event and current-task updates.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use scion_core::observation::Observation;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod workspace;

const APPLICATION_ID: i64 = 0x5343494f;

/// Public storage failures, retaining a clear source without printing credentials.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database file operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored observation is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported or foreign database")]
    UnsupportedDatabase,
    #[error("schema 1 requires state-upgrade")]
    UpgradeRequired,
    #[error("{0}")]
    Coordination(String),
    #[error("path overlap with task {task}: {path} and {occupied_path}")]
    PathOverlap {
        task: String,
        path: String,
        occupied_path: String,
    },
    #[error("delivery ID was already used with different observation data")]
    DeliveryConflict,
    #[error("invalid observation or delivery ID")]
    InvalidInput,
    #[error("task ID already belongs to a different specification")]
    TaskConflict,
}

/// Persisted immutable event, with currentness computed from the task's latest pointer.
#[derive(Clone, Debug, Serialize)]
pub struct StoredObservation {
    pub delivery_id: String,
    pub recorded_at: i64,
    pub current: bool,
    pub observation: Observation,
}

/// The event already exists on a replay; a replay must not move the current task backwards.
#[derive(Debug, Serialize)]
pub struct RecordResult {
    pub replayed: bool,
    pub record: StoredObservation,
}

/// Queued metadata survives restarts; there is no Agent execution in M1.
#[derive(Debug, Serialize)]
pub struct StoredTask {
    pub spec: scion_core::task::TaskSpec,
    pub state: &'static str,
    pub created_at: i64,
}

/// Single-file SQLite observation store. Only new or recognized Scion databases are accepted.
pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn create_task(
        &mut self,
        task: &scion_core::task::TaskSpec,
    ) -> Result<StoredTask, StoreError> {
        if !task.valid() {
            return Err(StoreError::InvalidInput);
        }
        let payload = serde_json::to_string(task)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT payload FROM task_specs WHERE task_id=?1",
                [&task.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(prior) = prior {
            if prior != payload {
                return Err(StoreError::TaskConflict);
            }
        } else {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| StoreError::InvalidInput)?
                .as_secs();
            let now = i64::try_from(now).map_err(|_| StoreError::InvalidInput)?;
            tx.execute(
                "INSERT INTO task_specs VALUES (?1,?2,?3)",
                params![task.id, payload, now],
            )?;
        }
        tx.commit()?;
        self.task(&task.id)?.ok_or(StoreError::InvalidInput)
    }
    pub fn task(&self, id: &str) -> Result<Option<StoredTask>, StoreError> {
        let row: Option<(String, i64)> = self
            .connection
            .query_row(
                "SELECT payload,created_at FROM task_specs WHERE task_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(payload, created_at)| {
            Ok(StoredTask {
                spec: serde_json::from_str(&payload)?,
                created_at,
                state: "queued",
            })
        })
        .transpose()
    }
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if !path.exists() {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let app: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if app == 0 && version == 0 {
            let tables: i64 = tx.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if tables != 0 {
                return Err(StoreError::UnsupportedDatabase);
            }
            tx.execute_batch("CREATE TABLE events (delivery_id TEXT PRIMARY KEY, repository TEXT NOT NULL, pr_number INTEGER NOT NULL, fingerprint TEXT NOT NULL, payload TEXT NOT NULL, recorded_at INTEGER NOT NULL);
                CREATE TABLE tasks (repository TEXT NOT NULL, pr_number INTEGER NOT NULL, current_event TEXT NOT NULL REFERENCES events(delivery_id), PRIMARY KEY(repository, pr_number));
                CREATE TABLE task_specs (task_id TEXT PRIMARY KEY, payload TEXT NOT NULL, created_at INTEGER NOT NULL);")?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.execute_batch(workspace::SCHEMA)?;
            tx.pragma_update(None, "user_version", 2)?;
        } else if app == APPLICATION_ID && version == 1 {
            return Err(StoreError::UpgradeRequired);
        } else if app != APPLICATION_ID || version != 2 {
            return Err(StoreError::UnsupportedDatabase);
        }
        tx.commit()?;
        Ok(Self { connection })
    }
    pub fn record(
        &mut self,
        delivery: &str,
        observation: &Observation,
    ) -> Result<RecordResult, StoreError> {
        if delivery.trim().is_empty()
            || delivery.len() > 256
            || delivery.chars().any(char::is_control)
            || observation.schema_version != 1
            || !scion_core::validation::valid_repository(&observation.revision.repository)
            || !scion_core::validation::valid_sha(&observation.revision.head_sha)
            || !scion_core::validation::valid_sha(&observation.revision.base_sha)
            || observation
                .revision
                .merge_sha
                .as_deref()
                .is_some_and(|sha| !scion_core::validation::valid_sha(sha))
        {
            return Err(StoreError::InvalidInput);
        }
        let pr = i64::try_from(observation.revision.pull_request)
            .map_err(|_| StoreError::InvalidInput)?;
        if pr <= 0 {
            return Err(StoreError::InvalidInput);
        }
        let payload = serde_json::to_string(observation)?;
        let fingerprint = format!("{:x}", Sha256::digest(payload.as_bytes()));
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT fingerprint FROM events WHERE delivery_id = ?1",
                [delivery],
                |r| r.get(0),
            )
            .optional()?;
        let replayed = if let Some(prior) = prior {
            if prior != fingerprint {
                return Err(StoreError::DeliveryConflict);
            }
            true
        } else {
            let recorded_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| StoreError::InvalidInput)?
                .as_secs();
            let recorded_at = i64::try_from(recorded_at).map_err(|_| StoreError::InvalidInput)?;
            tx.execute(
                "INSERT INTO events VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    delivery,
                    observation.revision.repository,
                    pr,
                    fingerprint,
                    payload,
                    recorded_at
                ],
            )?;
            tx.execute("INSERT INTO tasks VALUES (?1, ?2, ?3) ON CONFLICT(repository, pr_number) DO UPDATE SET current_event=excluded.current_event", params![observation.revision.repository, pr, delivery])?;
            false
        };
        let (recorded_at, payload, current): (i64, String, bool) = tx.query_row("SELECT e.recorded_at, e.payload, t.current_event=e.delivery_id FROM events e JOIN tasks t ON t.repository=e.repository AND t.pr_number=e.pr_number WHERE e.delivery_id=?1", [delivery], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        tx.commit()?;
        Ok(RecordResult {
            replayed,
            record: StoredObservation {
                delivery_id: delivery.into(),
                recorded_at,
                current,
                observation: serde_json::from_str(&payload)?,
            },
        })
    }
    pub fn history(&self, repository: &str, pr: u64) -> Result<Vec<StoredObservation>, StoreError> {
        let pr = i64::try_from(pr).map_err(|_| StoreError::InvalidInput)?;
        let mut stmt = self.connection.prepare("SELECT e.delivery_id,e.recorded_at,e.payload,t.current_event=e.delivery_id FROM events e JOIN tasks t ON t.repository=e.repository AND t.pr_number=e.pr_number WHERE e.repository=?1 AND e.pr_number=?2 ORDER BY e.rowid")?;
        let rows = stmt.query_map(params![repository, pr], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (delivery_id, recorded_at, payload, current) = row?;
            Ok(StoredObservation {
                delivery_id,
                recorded_at,
                current,
                observation: serde_json::from_str(&payload)?,
            })
        })
        .collect()
    }
}

mod attempt;
