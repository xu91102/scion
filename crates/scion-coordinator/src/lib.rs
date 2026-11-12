//! Coordinates workspace reservations and recoverable Git operations.

use scion_core::{
    validation::valid_repository,
    workspace::{Phase, Project, Workspace, valid_path, within},
};
use scion_git::{Git, Merge};
use scion_store::Store;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn digest(value: &impl Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn lock(root: &Path, name: &str) -> Result<File> {
    let path = root.join(name);
    if path
        .symlink_metadata()
        .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
    {
        return Err("invalid workspace lock path".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.try_lock()?;
    Ok(file)
}

pub struct Coordinator {
    store: Store,
    database: PathBuf,
}

impl Coordinator {
    pub fn open(database: &Path) -> Result<Self> {
        let database = match database.symlink_metadata() {
            Ok(_) => fs::canonicalize(database)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = database
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                fs::canonicalize(parent)?
                    .join(database.file_name().ok_or("database needs a file name")?)
            }
            Err(error) => return Err(error.into()),
        };
        if scion_git::inside_checkout(
            database
                .parent()
                .ok_or("database needs a parent directory")?,
        )? {
            return Err("workspace database must be outside Git checkouts".into());
        }
        let store = Store::open(&database)?;
        Ok(Self { store, database })
    }

    pub fn register(&self, mut project: Project) -> Result<Project> {
        if !valid_repository(&project.repository) {
            return Err("invalid repository identity".into());
        }
        project.repository.make_ascii_lowercase();
        project.source_checkout = fs::canonicalize(&project.source_checkout)?;
        let remote = scion_git::origin(&project.source_checkout, &project.repository)?;
        let root = &project.state_root;
        let name = root
            .file_name()
            .ok_or("state root needs a directory name")?;
        let parent = root
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = fs::canonicalize(parent)?;
        if scion_git::inside_checkout(&parent)? {
            return Err("state root must be outside Git checkouts".into());
        }
        project.state_root = parent.join(name);
        let root = &project.state_root;
        if root.starts_with(&project.source_checkout)
            || project.source_checkout.starts_with(root)
            || self.database.starts_with(root)
        {
            return Err("state root overlaps the source checkout or database".into());
        }
        if let Some(prior) = self.store.project(&project.repository)?
            && prior != project
        {
            return Err("repository already registered with different configuration".into());
        }
        create_private_dir(root)?;
        let _guard = lock(root, "registration.lock")?;
        let identity = self.identity(&project)?;
        let marker = root.join("project");
        if !marker.exists() {
            if fs::read_dir(root)?
                .any(|entry| entry.map_or(true, |e| e.file_name() != "registration.lock"))
            {
                return Err("state root contains unrelated files".into());
            }
            write_identity(root, &marker, &identity)?;
        }
        self.check_root(&project)?;
        let cache = root.join("objects.git");
        if !cache.exists() {
            Git::init(root)?;
        }
        Git::open(root, remote)?;
        self.store.register_project(&project)?;
        Ok(project)
    }

    fn identity(&self, project: &Project) -> Result<String> {
        digest(&(project, &self.database))
    }

    fn check_root(&self, project: &Project) -> Result<()> {
        let root = &project.state_root;
        let marker = root.join("project");
        if fs::canonicalize(root)? != *root
            || !marker.symlink_metadata()?.is_file()
            || fs::read_to_string(marker)? != self.identity(project)?
        {
            return Err("state directory identity changed".into());
        }
        Ok(())
    }

    fn git(&self, project: &Project) -> Result<Git> {
        self.check_root(project)?;
        let remote = scion_git::origin(&project.source_checkout, &project.repository)?;
        Ok(Git::open(&project.state_root, remote)?)
    }

    fn project(&self, repo: &str) -> Result<Project> {
        self.store
            .project(repo)?
            .ok_or_else(|| "project is not registered".into())
    }

    fn workspace(&self, task: &str) -> Result<Workspace> {
        self.store
            .workspace(task)?
            .ok_or_else(|| "workspace does not exist".into())
    }

    pub fn prepare(&mut self, task: &str, mut sparse: Vec<String>) -> Result<Workspace> {
        let spec = self.store.task(task)?.ok_or("task does not exist")?.spec;
        let repo = spec.repository.to_ascii_lowercase();
        let project = self.project(&repo)?;
        let git = self.git(&project)?;
        if spec
            .allowed_paths
            .iter()
            .chain(&sparse)
            .any(|path| !valid_path(path))
        {
            return Err("invalid workspace path scope".into());
        }
        sparse.sort();
        sparse.dedup();
        let id = digest(&(repo.as_str(), task))?;
        let _guard = lock(&project.state_root, &format!("{id}.lock"))?;
        let base = match self.store.workspace(task)? {
            Some(prior) => prior.base,
            None => git.target(&spec.target_branch)?,
        };
        let requested = Workspace {
            task: task.into(),
            repository: repo,
            target: spec.target_branch.clone(),
            base,
            branch: format!("scion/{id}"),
            path: project.state_root.join(&id),
            allowed: spec.allowed_paths.clone(),
            sparse,
            config_digest: digest(&project)?,
            spec_digest: digest(&spec)?,
            phase: Phase::Reserved,
            revision: 0,
            last_error: None,
        };
        let mut workspace = self.store.reserve(&requested)?;
        self.finish_prepare(&git, &mut workspace)?;
        Ok(workspace)
    }

    fn finish_prepare(&self, git: &Git, workspace: &mut Workspace) -> Result<()> {
        if workspace.phase == Phase::Reserved {
            self.store.advance(workspace, Phase::Preparing)?;
        }
        if workspace.phase == Phase::Preparing {
            if let Err(error) = git
                .fetch(&workspace.base)
                .and_then(|()| git.prepare(workspace))
            {
                self.store
                    .note_prepare_failure(workspace, error.to_string())?;
                return Err(error.into());
            }
            self.store.advance(workspace, Phase::Ready)?;
        }
        Ok(())
    }

    pub fn recover(&self) -> Result<Vec<Value>> {
        self.store
            .workspaces()?
            .into_iter()
            .filter(|w| matches!(w.phase, Phase::Reserved | Phase::Preparing))
            .map(|mut w| {
                let result = (|| {
                    let project = self.project(&w.repository)?;
                    let git = self.git(&project)?;
                    let _guard = self.lock_workspace(&project, &w)?;
                    w = self.workspace(&w.task)?;
                    self.finish_prepare(&git, &mut w)
                })();
                Ok(match result {
                    Ok(()) => json!({"workspace":w}),
                    Err(error) => {
                        json!({"workspace":w,"outcome":"BLOCKED","reason":error.to_string()})
                    }
                })
            })
            .collect()
    }

    fn lock_workspace(&self, project: &Project, w: &Workspace) -> Result<File> {
        let id = digest(&(w.repository.as_str(), w.task.as_str()))?;
        if w.path != project.state_root.join(&id)
            || w.branch != format!("scion/{id}")
            || w.config_digest != digest(project)?
        {
            return Err("workspace record identity changed".into());
        }
        lock(&project.state_root, &format!("{id}.lock"))
    }

    pub fn show(&self, task: &str) -> Result<Value> {
        let initial = self.workspace(task)?;
        let project = self.project(&initial.repository)?;
        let git = self.git(&project)?;
        let _guard = self.lock_workspace(&project, &initial)?;
        let w = self.workspace(task)?;
        if matches!(w.phase, Phase::Ready | Phase::Released) {
            let (head, dirty) = git.status(&w)?;
            Ok(json!({"workspace":w,"git_observed":true,"head":head,"dirty":dirty}))
        } else {
            Ok(json!({"workspace":w,"git_observed":false,"recovery_required":true}))
        }
    }

    pub fn release(&self, task: &str) -> Result<Workspace> {
        let w = self.workspace(task)?;
        let project = self.project(&w.repository)?;
        let git = self.git(&project)?;
        let _guard = self.lock_workspace(&project, &w)?;
        let mut w = self.workspace(task)?;
        if self.agent_attempts(task)?.last().is_some_and(|attempt| {
            matches!(
                attempt.phase,
                scion_core::attempt::Phase::Running | scion_core::attempt::Phase::CancelRequested
            )
        }) {
            return Err("Agent attempt must stop before workspace release".into());
        }
        if w.phase == Phase::Released {
            return Ok(w);
        }
        if w.phase != Phase::Ready {
            return Err("workspace must be recovered before release".into());
        }
        if !git.status(&w)?.1.is_empty() {
            return Err("dirty workspace cannot be released".into());
        }
        self.store.advance(&mut w, Phase::Released)?;
        Ok(w)
    }

    pub fn preflight(&self, tasks: &[String]) -> Result<Value> {
        let first = self.workspace(tasks.first().ok_or("at least one task is required")?)?;
        let project = self.project(&first.repository)?;
        let git = self.git(&project)?;
        let mut lock_order = tasks.to_vec();
        lock_order.sort();
        lock_order.dedup();
        if lock_order.len() != tasks.len() {
            return Err("duplicate task".into());
        }
        let mut guards = Vec::new();
        for task in &lock_order {
            let w = self.workspace(task)?;
            if w.repository != first.repository || w.target != first.target {
                return Err("preflight requires the same repository and target".into());
            }
            guards.push(self.lock_workspace(&project, &w)?);
        }
        let workspaces = tasks
            .iter()
            .map(|task| self.workspace(task))
            .collect::<Result<Vec<_>>>()?;
        let target = git.target(&first.target)?;
        git.fetch(&target)?;
        let mut report = self.compare(&git, &target, &workspaces)?;
        report["repository"] = json!(first.repository);
        report["target"] = json!(first.target);
        report["target_sha"] = json!(target);
        report["config_digest"] = json!(first.config_digest);
        report["order"] = json!(tasks);
        let id = self.store.save_preflight(&report)?;
        report["report_id"] = json!(id);
        Ok(report)
    }

    fn compare(&self, git: &Git, target: &str, workspaces: &[Workspace]) -> Result<Value> {
        let mut heads = Vec::new();
        for w in workspaces {
            if w.phase != Phase::Ready {
                return Ok(
                    json!({"outcome":"BLOCKED","task":w.task,"reason":"workspace is not ready"}),
                );
            }
            let (head, dirty) = match git.status(w) {
                Ok(status) => status,
                Err(scion_git::Error::InitializedSubmodule) => {
                    return Ok(json!({
                        "outcome":"BLOCKED","task":w.task,"reason":"initialized submodules require manual review"
                    }));
                }
                Err(error) => return Err(error.into()),
            };
            if !dirty.is_empty() {
                return Ok(json!({"outcome":"DIRTY","task":w.task,"head":head,"dirty":dirty}));
            }
            let paths = match git.changes(&w.base, &head) {
                Ok(paths) => paths,
                Err(scion_git::Error::SubmoduleChange) => {
                    return Ok(json!({
                        "outcome":"BLOCKED","task":w.task,"head":head,"reason":"submodule changes require manual review"
                    }));
                }
                Err(error) => return Err(error.into()),
            };
            let outside: Vec<_> = paths
                .iter()
                .filter(|path| {
                    !valid_path(path) || !w.allowed.iter().any(|prefix| within(path, prefix))
                })
                .collect();
            if !outside.is_empty() {
                return Ok(
                    json!({"outcome":"BLOCKED","task":w.task,"head":head,"outside_scope":outside}),
                );
            }
            heads.push(head);
        }
        let mut candidate = target.to_owned();
        let mut report = json!({"outcome":"CLEAN","heads":heads});
        for (w, head) in workspaces.iter().zip(&heads) {
            match git.combine(&candidate, head)? {
                Merge::Clean(commit) => candidate = commit,
                Merge::Conflict(paths) => {
                    report = json!({"outcome":"CONFLICT","heads":heads,"task":w.task,"conflict_paths":paths});
                    break;
                }
            }
        }
        if report["outcome"] == "CLEAN" {
            report["candidate_sha"] = json!(candidate);
        }
        let mut stale = git.target(&workspaces[0].target)? != target;
        for (w, head) in workspaces.iter().zip(&heads) {
            let (latest, dirty) = git.status(w)?;
            stale |= latest != *head || !dirty.is_empty();
        }
        if stale {
            report = json!({"outcome":"STALE","heads":heads});
        }
        Ok(report)
    }
}

fn create_private_dir(root: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if let Err(error) = builder.create(root)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(error.into());
    }
    if !root.symlink_metadata()?.is_dir() || fs::canonicalize(root)? != root {
        return Err("invalid state root".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if root.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err("state root must be private (0700)".into());
        }
    }
    Ok(())
}

fn write_identity(root: &Path, marker: &Path, value: &str) -> Result<()> {
    let temporary = root.join("project.pending");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary, marker)?;
    Ok(())
}

mod agent;
