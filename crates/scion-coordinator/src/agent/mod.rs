mod docker;
mod github;
mod snapshot;

use self::docker::Docker;
use super::{Coordinator, Result, create_private_dir, digest};
use scion_core::{
    attempt::{Attempt, Phase},
    workspace::Phase as WorkspacePhase,
};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn active(phase: Phase) -> bool {
    matches!(phase, Phase::Running | Phase::CancelRequested)
}

impl Coordinator {
    pub fn agent_attempts(&self, task: &str) -> Result<Vec<Attempt>> {
        self.store.enable_attempts()?;
        if self.store.task(task)?.is_none() {
            return Err("task does not exist".into());
        }
        Ok(self.store.attempts(task)?)
    }

    pub fn run_agent(&mut self, task: &str, image: &str) -> Result<Attempt> {
        let spec = self.store.task(task)?.ok_or("task does not exist")?.spec;
        let workspace = self.workspace(task)?;
        let project = self.project(&workspace.repository)?;
        let git = self.git(&project)?;
        let _guard = self.lock_workspace(&project, &workspace)?;
        let workspace = self.workspace(task)?;
        if workspace.phase != WorkspacePhase::Ready || !workspace.sparse.is_empty() {
            return Err("Agent execution requires a ready full workspace".into());
        }
        let previous = self.agent_attempts(task)?.pop();
        if let Some(mut row) = previous.clone() {
            if row.phase == Phase::Committed {
                return Ok(row);
            }
            if active(row.phase) {
                Docker::local(&row.daemon)?.remove(&row.key)?;
                row.phase = Phase::Failed;
                row.reason = Some("previous supervisor interrupted; container removed".into());
                return Ok(self.store.finish_attempt(row)?);
            }
        }
        let number = previous
            .map_or(Some(1), |row| row.number.checked_add(1))
            .filter(|n| *n <= spec.max_attempts)
            .ok_or("attempt budget exhausted")?;
        let (head, dirty) = git.status(&workspace)?;
        if head != workspace.base || !dirty.is_empty() {
            return Err("Agent execution requires a clean workspace at its frozen base".into());
        }
        let docker = Docker::discover()?;
        docker.check_image(image)?;
        let auth = auth_file()?;
        let key = digest(&(&self.database, task, number))?;
        let root = project.state_root.join(format!("attempt-{key}"));
        let work = root.join("work");
        let mut row = Attempt {
            task: task.into(),
            number,
            key,
            base: workspace.base.clone(),
            image: image.into(),
            credential_mounted: auth.is_some(),
            daemon: docker.endpoint.clone(),
            phase: Phase::Running,
            elapsed_ms: 0,
            exit_code: None,
            head: None,
            changed_paths: vec![],
            reason: None,
            pr: None,
        };
        self.store.start_attempt(&row, spec.max_attempts)?;
        let started = Instant::now();
        let result: Result<()> = (|| {
            if root.symlink_metadata().is_ok() {
                return Err("attempt directory already exists".into());
            }
            create_private_dir(&root)?;
            fs::create_dir(&work)?;
            git.export_snapshot(&row.base, &work)?;
            let before = snapshot::manifest(&work)?;
            let mut secrets = snapshot::secrets(auth.as_deref())?;
            let prompt = format!(
                "Implement this Scion task in /work. Modify only allowed paths. Run applicable checks. Do not create a Git repository or push. The task specification follows.\n{}\n",
                serde_json::to_string_pretty(&spec)?,
            );
            fs::write(root.join("task.json"), prompt)?;
            let seconds = spec.timeout_seconds.min(3600);
            let outcome = self.supervise(&docker, &row, image, &root, auth.as_deref(), seconds);
            // Never attest a terminal result while the container could still write the snapshot.
            docker.remove(&row.key)?;
            let (phase, exit, archive) = outcome?;
            row.exit_code = exit;
            row.phase = phase;
            if phase != Phase::Committed {
                row.reason = Some(
                    match phase {
                        Phase::Cancelled => "cancel requested",
                        Phase::TimedOut => "execution time budget exhausted",
                        _ => "Agent container exited unsuccessfully",
                    }
                    .into(),
                );
                return Ok(());
            }
            secrets.extend(snapshot::secrets(auth.as_deref())?);
            let output = root.join("output");
            fs::create_dir(&output)?;
            snapshot::extract(&archive, &output, &secrets)?;
            let after = snapshot::manifest(&output)?;
            let files = snapshot::changes(&before, &after, &workspace.allowed)?;
            let head =
                git.commit_snapshot(&row.base, &output, &root.join("index"), &files, &row.key)?;
            let actual = git.changes(&row.base, &head)?;
            let expected: std::collections::BTreeSet<_> =
                files.iter().map(|(path, _)| path).collect();
            if actual.iter().collect::<std::collections::BTreeSet<_>>() != expected {
                return Err("committed paths differ from validated Agent changes".into());
            }
            row.changed_paths = actual;
            row.head = Some(head);
            Ok(())
        })();
        if let Err(error) = result {
            docker.remove(&row.key)?;
            row.phase = Phase::Failed;
            row.head = None;
            row.reason = Some(error.to_string());
        }
        row.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(self.store.finish_attempt(row)?)
    }

    fn supervise(
        &self,
        docker: &Docker,
        row: &Attempt,
        image: &str,
        root: &Path,
        auth: Option<&Path>,
        seconds: u64,
    ) -> Result<(Phase, Option<i64>, Vec<u8>)> {
        let cancelled = || -> Result<bool> {
            Ok(self
                .agent_attempts(&row.task)?
                .last()
                .is_some_and(|r| r.key == row.key && r.phase == Phase::CancelRequested))
        };
        if cancelled()? {
            return Ok((Phase::Cancelled, None, vec![]));
        }
        let deadline = Instant::now() + Duration::from_secs(seconds);
        docker.create(&row.key, image, root, auth, seconds)?;
        if cancelled()? {
            return Ok((Phase::Cancelled, None, vec![]));
        }
        if Instant::now() >= deadline {
            return Ok((Phase::TimedOut, None, vec![]));
        }
        let mut attached = docker.start(&row.key)?;
        loop {
            if cancelled()? {
                return Ok((Phase::Cancelled, None, vec![]));
            }
            if Instant::now() >= deadline {
                return Ok((Phase::TimedOut, None, vec![]));
            }
            if let Some((exit, archive)) = attached.poll()? {
                let phase = if exit == Some(0) {
                    Phase::Committed
                } else {
                    Phase::Failed
                };
                return Ok((phase, exit.map(i64::from), archive));
            }
            thread::sleep(Duration::from_millis(150));
        }
    }

    pub fn cancel_agent(&mut self, task: &str) -> Result<Attempt> {
        let workspace = self.workspace(task)?;
        let project = self.project(&workspace.repository)?;
        self.check_root(&project)?;
        self.store.enable_attempts()?;
        let row = self.store.cancel_attempt(task)?;
        if row.phase != Phase::CancelRequested {
            return Ok(row);
        }
        // A live supervisor owns the lock and acknowledges after terminating the container.
        let Ok(_guard) = self.lock_workspace(&project, &workspace) else {
            return Ok(row);
        };
        let mut current = self
            .agent_attempts(task)?
            .pop()
            .ok_or("attempt disappeared")?;
        if current.key == row.key && current.phase == Phase::CancelRequested {
            Docker::local(&current.daemon)?.remove(&current.key)?;
            current.phase = Phase::Cancelled;
            current.reason = Some("cancel requested".into());
            return Ok(self.store.finish_attempt(current)?);
        }
        Ok(current)
    }

    pub fn publish_agent(&mut self, task: &str, reviewed: Option<&str>) -> Result<Attempt> {
        let workspace = self.workspace(task)?;
        let project = self.project(&workspace.repository)?;
        let git = self.git(&project)?;
        let _guard = self.lock_workspace(&project, &workspace)?;
        let workspace = self.workspace(task)?;
        let attempt = self
            .agent_attempts(task)?
            .pop()
            .ok_or("task has no Agent attempt")?;
        let head = attempt
            .head
            .as_deref()
            .ok_or("task has no committed candidate")?;
        if workspace.phase != WorkspacePhase::Ready
            || attempt.phase != Phase::Committed
            || attempt.base != workspace.base
        {
            return Err("task is not ready for publication".into());
        }
        if attempt.credential_mounted && reviewed != Some(head) {
            return Err("provider credentials were mounted; inspect the candidate and pass --reviewed-head <sha>".into());
        }
        if let Some(pr) = github::reconcile(&workspace, head)? {
            return Ok(self.store.attach_pr(task, head, pr)?);
        }
        if attempt.pr.is_some() {
            return Err("recorded task PR is missing or changed".into());
        }
        if git.target(&workspace.target)? != workspace.base {
            return Err(
                "target moved since the Agent snapshot; create a new task on the current base"
                    .into(),
            );
        }
        git.push_candidate(&workspace.branch, head)?;
        if git.target(&workspace.target)? != workspace.base {
            return Err("target moved during publication".into());
        }
        let receipt = match github::reconcile(&workspace, head)? {
            Some(pr) => pr,
            None => {
                if git.target(&workspace.target)? != workspace.base {
                    return Err("target moved before creating the draft PR".into());
                }
                github::create(&workspace, head)?
            }
        };
        Ok(self.store.attach_pr(task, head, receipt)?)
    }
}

fn auth_file() -> Result<Option<PathBuf>> {
    let Some(path) = std::env::var_os("SCION_AGENT_AUTH_FILE") else {
        return Ok(None);
    };
    let path = fs::canonicalize(path)?;
    let meta = path.metadata()?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return Err("Agent auth must be a regular credential file within 1 MiB".into());
    }
    Ok(Some(path))
}
