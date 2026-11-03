use super::{Store, StoreError};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use scion_core::attempt::{Attempt, Phase, PullRequest};

impl Store {
    // Additive to schema 2. Earlier readers continue to use their original tables.
    pub fn enable_attempts(&self) -> Result<(), StoreError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_attempts (
            task TEXT NOT NULL REFERENCES task_specs(task_id),
            number INTEGER NOT NULL,
            payload TEXT NOT NULL,
            PRIMARY KEY(task, number)
        );",
        )?;
        Ok(())
    }

    pub fn attempts(&self, task: &str) -> Result<Vec<Attempt>, StoreError> {
        let mut stmt = self
            .connection
            .prepare("SELECT payload FROM agent_attempts WHERE task=?1 ORDER BY number")?;
        let rows = stmt.query_map([task], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn start_attempt(&mut self, attempt: &Attempt, maximum: u32) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior = latest(&tx, &attempt.task)?;
        if attempt.phase != Phase::Running
            || prior.as_ref().is_some_and(|row| {
                matches!(
                    row.phase,
                    Phase::Running | Phase::CancelRequested | Phase::Committed
                )
            })
            || Some(attempt.number) != prior.map_or(Some(1), |row| row.number.checked_add(1))
            || attempt.number > maximum
        {
            return Err(StoreError::Coordination(
                "attempt budget exhausted or task already active/committed".into(),
            ));
        }
        tx.execute(
            "INSERT INTO agent_attempts VALUES (?1,?2,?3)",
            params![
                attempt.task,
                attempt.number,
                serde_json::to_string(attempt)?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn cancel_attempt(&mut self, task: &str) -> Result<Attempt, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut row = latest(&tx, task)?.ok_or(StoreError::InvalidInput)?;
        if row.phase == Phase::Running {
            row.phase = Phase::CancelRequested;
            save(&tx, &row)?;
        }
        tx.commit()?;
        Ok(row)
    }

    pub fn finish_attempt(&mut self, mut result: Attempt) -> Result<Attempt, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = latest(&tx, &result.task)?.ok_or(StoreError::InvalidInput)?;
        if current.key != result.key
            || !matches!(current.phase, Phase::Running | Phase::CancelRequested)
            || matches!(result.phase, Phase::Running | Phase::CancelRequested)
        {
            return Err(StoreError::Coordination(
                "attempt is no longer active".into(),
            ));
        }
        if current.phase == Phase::CancelRequested {
            result.phase = Phase::Cancelled;
            result.reason = Some("cancel requested".into());
            result.head = None;
        }
        save(&tx, &result)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn attach_pr(
        &mut self,
        task: &str,
        head: &str,
        pr: PullRequest,
    ) -> Result<Attempt, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut row = latest(&tx, task)?.ok_or(StoreError::InvalidInput)?;
        if row.phase != Phase::Committed
            || row.head.as_deref() != Some(head)
            || row
                .pr
                .as_ref()
                .is_some_and(|previous| previous.number != pr.number)
        {
            return Err(StoreError::Coordination(
                "publication identity changed".into(),
            ));
        }
        row.pr = Some(pr);
        save(&tx, &row)?;
        tx.commit()?;
        Ok(row)
    }
}

fn latest(conn: &rusqlite::Connection, task: &str) -> Result<Option<Attempt>, StoreError> {
    let row: Option<String> = conn
        .query_row(
            "SELECT payload FROM agent_attempts WHERE task=?1 ORDER BY number DESC LIMIT 1",
            [task],
            |row| row.get(0),
        )
        .optional()?;
    row.map(|data| Ok(serde_json::from_str(&data)?)).transpose()
}

fn save(conn: &rusqlite::Connection, row: &Attempt) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE agent_attempts SET payload=?1 WHERE task=?2 AND number=?3",
        params![serde_json::to_string(row)?, row.task, row.number],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use scion_core::task::TaskSpec;

    fn ledger() -> Store {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE task_specs(task_id TEXT PRIMARY KEY,payload TEXT,created_at INTEGER);").unwrap();
        let mut store = Store { connection };
        store.enable_attempts().unwrap();
        store
            .create_task(&TaskSpec {
                id: "test".into(),
                repository: "example/scion".into(),
                target_branch: "main".into(),
                goal: "fixture".into(),
                allowed_paths: vec!["src".into()],
                acceptance_criteria: vec!["fixture".into()],
                owner: "fixture".into(),
                max_attempts: 1,
                timeout_seconds: 3,
            })
            .unwrap();
        store
    }

    fn running() -> Attempt {
        Attempt {
            task: "test".into(),
            number: 1,
            key: "attempt-1".into(),
            base: "base".into(),
            image: "image".into(),
            credential_mounted: false,
            daemon: "daemon".into(),
            phase: Phase::Running,
            elapsed_ms: 0,
            exit_code: None,
            head: None,
            changed_paths: vec![],
            reason: None,
            pr: None,
        }
    }

    #[test]
    fn cancellation_wins_over_a_concurrent_candidate_and_keeps_budget() {
        let mut store = ledger();
        let mut row = running();
        store.start_attempt(&row, 1).unwrap();
        assert!(store.start_attempt(&row, 1).is_err());
        assert_eq!(
            store.cancel_attempt("test").unwrap().phase,
            Phase::CancelRequested
        );
        row.phase = Phase::Committed;
        row.head = Some("late-commit".into());
        let result = store.finish_attempt(row).unwrap();
        assert_eq!(result.phase, Phase::Cancelled);
        assert!(result.head.is_none());
        let mut retry = running();
        retry.number = 2;
        assert!(store.start_attempt(&retry, 1).is_err());
    }
}
