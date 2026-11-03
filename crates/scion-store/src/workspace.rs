use super::{APPLICATION_ID, Store, StoreError};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use scion_core::workspace::{Phase, Project, Workspace, within};
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) const SCHEMA: &str = "
CREATE TABLE projects (repository TEXT PRIMARY KEY, payload TEXT NOT NULL);
CREATE TABLE workspaces (
    task TEXT PRIMARY KEY REFERENCES task_specs(task_id),
    repository TEXT NOT NULL REFERENCES projects(repository),
    path TEXT NOT NULL UNIQUE,
    branch TEXT NOT NULL,
    revision INTEGER NOT NULL,
    payload TEXT NOT NULL,
    UNIQUE(repository, branch)
);
CREATE TABLE preflights (id INTEGER PRIMARY KEY, payload TEXT NOT NULL);
";

impl Store {
    /// Upgrade only during an operator-controlled maintenance window.
    pub fn upgrade(path: &Path) -> Result<Option<PathBuf>, StoreError> {
        let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let app: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match (app, version) {
            (APPLICATION_ID, 2) => return Ok(None),
            (APPLICATION_ID, 1) => {}
            _ => return Err(StoreError::UnsupportedDatabase),
        }
        for query in [
            "SELECT task_id,payload,created_at FROM task_specs LIMIT 0",
            "SELECT delivery_id,repository,pr_number,fingerprint,payload,recorded_at FROM events LIMIT 0",
            "SELECT repository,pr_number,current_event FROM tasks LIMIT 0",
        ] {
            conn.prepare(query)?;
        }
        let mut backup = path.as_os_str().to_owned();
        backup.push(".schema1-backup");
        let backup = PathBuf::from(backup);
        let backup_name = backup.to_str().ok_or(StoreError::InvalidInput)?;
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let backup_file = options.open(&backup)?;
        conn.execute("VACUUM INTO ?1", [backup_name])?;
        let copy = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let integrity: String = copy.pragma_query_value(None, "integrity_check", |r| r.get(0))?;
        if integrity != "ok" {
            return Err(StoreError::Coordination(
                "backup integrity check failed".into(),
            ));
        }
        backup_file.sync_all()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != 1 {
            return Err(StoreError::UnsupportedDatabase);
        }
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
        Ok(Some(backup))
    }

    pub fn register_project(&self, project: &Project) -> Result<(), StoreError> {
        let payload = serde_json::to_string(project)?;
        self.connection.execute(
            "INSERT INTO projects VALUES (?1,?2) ON CONFLICT(repository) DO NOTHING",
            params![project.repository, payload],
        )?;
        if self.project(&project.repository)?.as_ref() != Some(project) {
            return Err(StoreError::Coordination(
                "repository already registered with different configuration".into(),
            ));
        }
        Ok(())
    }

    pub fn project(&self, repo: &str) -> Result<Option<Project>, StoreError> {
        read_payload(
            &self.connection,
            "SELECT payload FROM projects WHERE repository=?1",
            repo,
        )
    }

    pub fn workspace(&self, task: &str) -> Result<Option<Workspace>, StoreError> {
        read_payload(
            &self.connection,
            "SELECT payload FROM workspaces WHERE task=?1",
            task,
        )
    }

    pub fn workspaces(&self) -> Result<Vec<Workspace>, StoreError> {
        let mut stmt = self
            .connection
            .prepare("SELECT payload FROM workspaces ORDER BY task")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn reserve(&mut self, requested: &Workspace) -> Result<Workspace, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = read_payload::<Workspace>(
            &tx,
            "SELECT payload FROM workspaces WHERE task=?1",
            &requested.task,
        )? {
            if prior.sparse != requested.sparse
                || prior.config_digest != requested.config_digest
                || prior.spec_digest != requested.spec_digest
            {
                return Err(StoreError::Coordination(
                    "task already reserved with different parameters".into(),
                ));
            }
            return Ok(prior);
        }
        {
            let mut stmt = tx.prepare("SELECT payload FROM workspaces WHERE repository=?1")?;
            for row in stmt.query_map([&requested.repository], |row| row.get::<_, String>(0))? {
                let active: Workspace = serde_json::from_str(&row?)?;
                if active.phase == Phase::Released {
                    continue;
                }
                for path in &requested.allowed {
                    if let Some(other) = active
                        .allowed
                        .iter()
                        .find(|other| within(path, other) || within(other, path))
                    {
                        return Err(StoreError::PathOverlap {
                            task: active.task.clone(),
                            path: path.clone(),
                            occupied_path: other.clone(),
                        });
                    }
                }
            }
        }
        tx.execute(
            "INSERT INTO workspaces VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                requested.task,
                requested.repository,
                requested.path.to_str().ok_or(StoreError::InvalidInput)?,
                requested.branch,
                requested.revision,
                serde_json::to_string(requested)?
            ],
        )?;
        tx.commit()?;
        Ok(requested.clone())
    }

    pub fn advance(&self, workspace: &mut Workspace, phase: Phase) -> Result<(), StoreError> {
        if !matches!(
            (workspace.phase, phase),
            (Phase::Reserved, Phase::Preparing)
                | (Phase::Preparing, Phase::Ready)
                | (Phase::Ready, Phase::Released)
        ) {
            return Err(StoreError::InvalidInput);
        }
        self.update_workspace(workspace, phase, None)
    }

    pub fn note_prepare_failure(
        &self,
        workspace: &mut Workspace,
        reason: String,
    ) -> Result<(), StoreError> {
        if workspace.phase != Phase::Preparing {
            return Err(StoreError::InvalidInput);
        }
        self.update_workspace(workspace, Phase::Preparing, Some(reason))
    }

    fn update_workspace(
        &self,
        workspace: &mut Workspace,
        phase: Phase,
        reason: Option<String>,
    ) -> Result<(), StoreError> {
        let mut next = workspace.clone();
        next.phase = phase;
        next.last_error = reason;
        next.revision += 1;
        let updated = self.connection.execute(
            "UPDATE workspaces SET revision=?1,payload=?2 WHERE task=?3 AND revision=?4",
            params![
                next.revision,
                serde_json::to_string(&next)?,
                workspace.task,
                workspace.revision
            ],
        )?;
        if updated != 1 {
            return Err(StoreError::Coordination(
                "workspace revision changed".into(),
            ));
        }
        *workspace = next;
        Ok(())
    }

    pub fn save_preflight(&self, report: &serde_json::Value) -> Result<i64, StoreError> {
        self.connection.execute(
            "INSERT INTO preflights(payload) VALUES (?1)",
            [report.to_string()],
        )?;
        Ok(self.connection.last_insert_rowid())
    }
}

fn read_payload<T: serde::de::DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    key: &str,
) -> Result<Option<T>, StoreError> {
    let value: Option<String> = conn.query_row(sql, [key], |row| row.get(0)).optional()?;
    value
        .map(|payload| Ok(serde_json::from_str(&payload)?))
        .transpose()
}
