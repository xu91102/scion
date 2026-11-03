//! Managed Git object storage and worktrees.

mod agent;
mod process;
use scion_core::{validation::valid_sha, workspace::Workspace};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Git {0} failed with status {1:?}")]
    Command(&'static str, Option<i32>),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("local Git operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Git returned unsupported non-UTF-8 metadata")]
    Encoding(#[from] std::string::FromUtf8Error),
    #[error("submodule changes require manual review")]
    SubmoduleChange,
    #[error("initialized submodules require manual review")]
    InitializedSubmodule,
}

fn args<'a>(values: &[&'a str]) -> Vec<&'a OsStr> {
    values.iter().copied().map(OsStr::new).collect()
}

fn run(dir: &Path, op: &'static str, values: &[&str], net: bool) -> Result<Vec<u8>, Error> {
    checked(dir, op, &args(values), net)
}

fn checked(dir: &Path, op: &'static str, values: &[&OsStr], net: bool) -> Result<Vec<u8>, Error> {
    let (status, bytes) = process::run(dir, values, net)?;
    if status == Some(0) {
        Ok(bytes)
    } else {
        Err(Error::Command(op, status))
    }
}

fn line(bytes: Vec<u8>) -> Result<String, Error> {
    Ok(String::from_utf8(bytes)?
        .trim_end_matches(['\n', '\r'])
        .to_owned())
}

fn sha(bytes: Vec<u8>) -> Result<String, Error> {
    let value = line(bytes)?;
    if valid_sha(&value) {
        Ok(value)
    } else {
        Err(Error::Invalid("invalid Git object identity"))
    }
}

/// Read only the source repository's configured GitHub identity.
pub fn origin(source: &Path, repository: &str) -> Result<String, Error> {
    let top = line(run(
        source,
        "source root",
        &["rev-parse", "--show-toplevel"],
        false,
    )?)?;
    if fs::canonicalize(top)? != source {
        return Err(Error::Invalid(
            "source checkout must be its Git worktree root",
        ));
    }
    let origin = line(run(
        source,
        "source origin",
        &["config", "--local", "--get", "remote.origin.url"],
        false,
    )?)?;
    for prefix in [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ] {
        if let Some(name) = origin.strip_prefix(prefix)
            && name
                .strip_suffix(".git")
                .unwrap_or(name)
                .eq_ignore_ascii_case(repository)
        {
            return Ok(format!("{prefix}{repository}.git"));
        }
    }
    Err(Error::Invalid(
        "origin must match the registered GitHub repository",
    ))
}

pub struct Git {
    cache: PathBuf,
    remote: String,
}

pub enum Merge {
    Clean(String),
    Conflict(Vec<String>),
}

impl Git {
    pub fn open(root: &Path, remote: String) -> Result<Self, Error> {
        let cache = root.join("objects.git");
        if cache.symlink_metadata()?.file_type().is_symlink()
            || line(run(
                &cache,
                "cache identity",
                &["rev-parse", "--is-bare-repository"],
                false,
            )?)? != "true"
            || fs::canonicalize(line(run(
                &cache,
                "cache path",
                &["rev-parse", "--absolute-git-dir"],
                false,
            )?)?)?
                != cache
        {
            return Err(Error::Invalid("invalid managed object cache"));
        }
        check_config(&cache)?;
        for relative in ["info/grafts", "shallow"] {
            match cache.join(relative).symlink_metadata() {
                Ok(_) => {
                    return Err(Error::Invalid(
                        "managed cache must have unmodified ancestry",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Self { cache, remote })
    }

    pub fn init(root: &Path) -> Result<(), Error> {
        run(
            root,
            "initialize cache",
            &["init", "--bare", "--template=", "objects.git"],
            false,
        )?;
        Ok(())
    }

    pub fn target(&self, branch: &str) -> Result<String, Error> {
        let reference = format!("refs/heads/{branch}");
        run(
            &self.cache,
            "validate target",
            &["check-ref-format", &reference],
            false,
        )?;
        let row = line(run(
            &self.cache,
            "read target",
            &[
                "ls-remote",
                "--exit-code",
                "--refs",
                &self.remote,
                &reference,
            ],
            true,
        )?)?;
        let (oid, found) = row
            .split_once('\t')
            .ok_or(Error::Invalid("invalid target response"))?;
        if found != reference || !valid_sha(oid) {
            return Err(Error::Invalid("invalid target identity"));
        }
        Ok(oid.into())
    }

    pub fn fetch(&self, sha: &str) -> Result<(), Error> {
        if !valid_sha(sha) {
            return Err(Error::Invalid("invalid fetch identity"));
        }
        let object = format!("{sha}^{{commit}}");
        if process::run(&self.cache, &args(&["cat-file", "-e", &object]), false)?.0 == Some(0) {
            return Ok(());
        }
        run(
            &self.cache,
            "fetch",
            &[
                "fetch",
                "--no-write-fetch-head",
                "--no-tags",
                "--no-recurse-submodules",
                &self.remote,
                sha,
            ],
            true,
        )?;
        Ok(())
    }

    pub fn prepare(&self, workspace: &Workspace) -> Result<(), Error> {
        if workspace
            .path
            .symlink_metadata()
            .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
        {
            let reference = format!("refs/heads/{}", workspace.branch);
            let (code, _) = process::run(
                &self.cache,
                &args(&["show-ref", "--verify", "--quiet", &reference]),
                false,
            )?;
            let path = workspace.path.as_os_str();
            let mut command = args(&["worktree", "add", "--no-track"]);
            match code {
                Some(1) => {
                    command.extend(args(&["-b", &workspace.branch, "--"]));
                    command.extend([path, OsStr::new(&workspace.base)]);
                }
                Some(0) => {
                    let found = sha(run(
                        &self.cache,
                        "recover branch",
                        &["rev-parse", &reference],
                        false,
                    )?)?;
                    if found != workspace.base {
                        return Err(Error::Invalid("managed branch has moved"));
                    }
                    command.push(OsStr::new("--"));
                    command.extend([path, OsStr::new(&workspace.branch)]);
                }
                _ => return Err(Error::Command("check managed branch", code)),
            }
            checked(&self.cache, "create worktree", &command, false)?;
        }
        let (head, dirty) = self.status(workspace)?;
        if head != workspace.base || !dirty.is_empty() {
            return Err(Error::Invalid("partially prepared worktree has changes"));
        }
        if !workspace.sparse.is_empty() {
            let mut command = vec!["sparse-checkout", "set", "--cone", "--"];
            command.extend(workspace.sparse.iter().map(String::as_str));
            run(
                &workspace.path,
                "configure sparse worktree",
                &command,
                false,
            )?;
        }
        Ok(())
    }

    pub fn status(&self, workspace: &Workspace) -> Result<(String, Vec<String>), Error> {
        let path = &workspace.path;
        if fs::canonicalize(path)? != *path {
            return Err(Error::Invalid("worktree path identity changed"));
        }
        let top = line(run(
            path,
            "worktree root",
            &["rev-parse", "--show-toplevel"],
            false,
        )?)?;
        let common = line(run(
            path,
            "worktree cache",
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            false,
        )?)?;
        let branch = line(run(
            path,
            "worktree branch",
            &["symbolic-ref", "HEAD"],
            false,
        )?)?;
        if Path::new(&top) != path
            || fs::canonicalize(common)? != self.cache
            || branch != format!("refs/heads/{}", workspace.branch)
        {
            return Err(Error::Invalid("worktree identity changed"));
        }
        check_config(path)?;
        let head = sha(run(
            path,
            "worktree head",
            &["rev-parse", "HEAD^{commit}"],
            false,
        )?)?;
        let mut dirty = inspect_index(path)?;
        let raw = run(
            path,
            "worktree status",
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--no-renames",
                "--untracked-files=all",
                "--ignore-submodules=dirty",
            ],
            false,
        )?;
        dirty.extend(
            String::from_utf8(raw)?
                .split_terminator('\0')
                .map(str::to_owned),
        );
        Ok((head, dirty))
    }

    pub fn changes(&self, base: &str, head: &str) -> Result<Vec<String>, Error> {
        if !valid_sha(base) || !valid_sha(head) {
            return Err(Error::Invalid("invalid diff identity"));
        }
        run(
            &self.cache,
            "validate ancestry",
            &["merge-base", "--is-ancestor", base, head],
            false,
        )?;
        let data = run(
            &self.cache,
            "read changes",
            &[
                "diff",
                "--raw",
                "--no-ext-diff",
                "--no-textconv",
                "-z",
                "--no-renames",
                "--ignore-submodules=none",
                base,
                head,
                "--",
            ],
            false,
        )?;
        let text = String::from_utf8(data)?;
        let mut fields = text.split_terminator('\0');
        let mut paths = Vec::new();
        while let Some(meta) = fields.next() {
            let path = fields
                .next()
                .ok_or(Error::Invalid("invalid Git diff output"))?;
            if meta
                .trim_start_matches(':')
                .split_ascii_whitespace()
                .take(2)
                .any(|mode| mode == "160000")
            {
                return Err(Error::SubmoduleChange);
            }
            paths.push(path.to_owned());
        }
        Ok(paths)
    }

    pub fn combine(&self, base: &str, head: &str) -> Result<Merge, Error> {
        if !valid_sha(base) || !valid_sha(head) {
            return Err(Error::Invalid("invalid merge identity"));
        }
        if base == head {
            return Ok(Merge::Clean(base.into()));
        }
        let (code, raw) = process::run(
            &self.cache,
            &args(&[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "-z",
                base,
                head,
            ]),
            false,
        )?;
        let output = String::from_utf8(raw)?;
        let mut fields = output.split_terminator('\0');
        let tree = fields.next().unwrap_or_default().trim_end();
        match code {
            Some(0) if valid_sha(tree) => {
                let commit = sha(run(
                    &self.cache,
                    "build synthetic candidate",
                    &[
                        "-c",
                        "user.name=Scion preflight",
                        "-c",
                        "user.email=scion@example.invalid",
                        "commit-tree",
                        tree,
                        "-p",
                        base,
                        "-p",
                        head,
                        "-m",
                        "Scion preflight",
                    ],
                    false,
                )?)?;
                Ok(Merge::Clean(commit))
            }
            Some(1) => Ok(Merge::Conflict(
                fields
                    .take_while(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )),
            _ => Err(Error::Command("preflight merge", code)),
        }
    }
}

pub fn inside_checkout(path: &Path) -> Result<bool, Error> {
    for parent in path.ancestors() {
        match parent.join(".git").symlink_metadata() {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let (status, output) =
        process::run(path, &args(&["rev-parse", "--is-inside-work-tree"]), false)?;
    match status {
        Some(0) => Ok(line(output)? == "true"),
        Some(128) => Ok(false),
        _ => Err(Error::Command("inspect state parent", status)),
    }
}

fn inspect_index(path: &Path) -> Result<Vec<String>, Error> {
    let flags = String::from_utf8(run(
        path,
        "inspect index flags",
        &["ls-files", "-v", "--stage", "-z"],
        false,
    )?)?;
    let mut dirty = Vec::new();
    for entry in flags.split_terminator('\0') {
        let (metadata, name) = entry
            .split_once('\t')
            .ok_or(Error::Invalid("invalid index metadata"))?;
        let (flag, metadata) = metadata
            .split_once(' ')
            .ok_or(Error::Invalid("invalid index metadata"))?;
        let entry_path = path.join(name);
        if metadata.starts_with("160000 ") && entry_exists(&entry_path.join(".git"))? {
            return Err(Error::InitializedSubmodule);
        }
        if flag.as_bytes().first().is_some_and(u8::is_ascii_lowercase) {
            dirty.push(format!("assume-unchanged: {name}"));
        } else if flag == "S" && entry_exists(&entry_path)? {
            dirty.push(format!("materialized skip-worktree: {name}"));
        }
    }
    Ok(dirty)
}

fn entry_exists(path: &Path) -> Result<bool, Error> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

fn check_config(dir: &Path) -> Result<(), Error> {
    // The operator's global authentication is available to network commands.
    // Managed repository and worktree configuration cannot add executable drivers
    // or override how those commands reach the registered upstream.
    for config in ["config", "config.worktree"] {
        let path = line(run(
            dir,
            "locate managed Git config",
            &["rev-parse", "--path-format=absolute", "--git-path", config],
            false,
        )?)?;
        match Path::new(&path).symlink_metadata() {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                return Err(Error::Invalid(
                    "managed Git configuration is not a regular file",
                ));
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && config == "config.worktree" =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
        let keys = String::from_utf8(run(
            dir,
            "inspect managed Git config",
            &[
                "config",
                "--file",
                &path,
                "--no-includes",
                "--name-only",
                "--list",
                "-z",
            ],
            false,
        )?)?;
        for name in keys.split_terminator('\0') {
            let key = name.to_ascii_lowercase();
            if [
                "include.",
                "includeif.",
                "filter.",
                "url.",
                "credential.",
                "http.",
                "protocol.",
            ]
            .iter()
            .any(|prefix| key.starts_with(prefix))
                || [
                    "core.sshcommand",
                    "core.gitproxy",
                    "core.askpass",
                    "diff.external",
                ]
                .contains(&key.as_str())
                || (key.starts_with("merge.") && key.ends_with(".driver"))
                || (key.starts_with("diff.")
                    && (key.ends_with(".command") || key.ends_with(".textconv")))
            {
                return Err(Error::Invalid(
                    "managed Git configuration contains unsupported executable or transport settings",
                ));
            }
        }
    }
    Ok(())
}
