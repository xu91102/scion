use super::{Error, Git, args, line, process, run, sha};
use scion_core::{validation::valid_sha, workspace::valid_path};
use std::{fs, path::Path};

impl Git {
    /// Export the exact committed bytes, without checkout filters or repository metadata.
    pub fn export_snapshot(&self, base: &str, output: &Path) -> Result<(), Error> {
        if !valid_sha(base) {
            return Err(Error::Invalid("invalid snapshot base"));
        }
        let listing = String::from_utf8(run(
            &self.cache,
            "list snapshot",
            &["ls-tree", "-r", "-z", base],
            false,
        )?)?;
        let mut total = 0usize;
        for (number, record) in listing.split_terminator('\0').enumerate() {
            let (meta, path) = record
                .split_once('\t')
                .ok_or(Error::Invalid("invalid snapshot tree"))?;
            let (mode, oid) = meta
                .split_once(" blob ")
                .ok_or(Error::Invalid("snapshot requires regular files"))?;
            if !matches!(mode, "100644" | "100755")
                || !valid_sha(oid)
                || !valid_path(path)
                || number >= 10_000
            {
                return Err(Error::Invalid("unsupported snapshot tree entry"));
            }
            let file = output.join(path);
            let data = run(
                &self.cache,
                "read snapshot object",
                &["cat-file", "blob", oid],
                false,
            )?;
            total += data.len();
            if total > 64 * 1024 * 1024 {
                return Err(Error::Invalid("snapshot exceeds 64 MiB"));
            }
            fs::create_dir_all(
                file.parent()
                    .ok_or(Error::Invalid("invalid snapshot path"))?,
            )?;
            // create_new also rejects colliding names on case-insensitive filesystems.
            use std::io::Write;
            let mut handle = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)?;
            handle.write_all(&data)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                handle.set_permissions(fs::Permissions::from_mode(if mode == "100755" {
                    0o755
                } else {
                    0o644
                }))?;
            }
        }
        Ok(())
    }

    /// Build a candidate with a private index and pin its object in the managed cache.
    pub fn commit_snapshot(
        &self,
        base: &str,
        root: &Path,
        index: &Path,
        files: &[(String, Option<bool>)],
        attempt: &str,
    ) -> Result<String, Error> {
        if !valid_sha(base) || !valid_sha(attempt) || index.exists() || files.is_empty() {
            return Err(Error::Invalid("invalid attempt index or revision"));
        }
        let run_index = |command: &[&str]| {
            let (status, data) =
                process::run_index(&self.cache, &args(command), false, Some(index))?;
            if status != Some(0) {
                return Err(Error::Command("build candidate index", status));
            }
            Ok(data)
        };
        run_index(&["read-tree", base])?;
        let order = files
            .iter()
            .filter(|(_, mode)| mode.is_none())
            .chain(files.iter().filter(|(_, mode)| mode.is_some()));
        for (path, executable) in order {
            if !valid_path(path) {
                return Err(Error::Invalid("invalid candidate path"));
            }
            if let Some(executable) = executable {
                let file = root.join(path);
                let file = file
                    .to_str()
                    .ok_or(Error::Invalid("invalid UTF-8 snapshot location"))?;
                let oid = sha(run(
                    &self.cache,
                    "write candidate blob",
                    &["hash-object", "-w", "--no-filters", "--", file],
                    false,
                )?)?;
                let mode = if *executable { "100755" } else { "100644" };
                run_index(&["update-index", "--add", "--cacheinfo", mode, &oid, path])?;
            } else {
                run_index(&["update-index", "--force-remove", "--", path])?;
            }
        }
        let tree = sha(run_index(&["write-tree"])?)?;
        let head = sha(run(
            &self.cache,
            "create task commit",
            &[
                "-c",
                "user.name=Scion Agent",
                "-c",
                "user.email=scion@example.invalid",
                "commit-tree",
                &tree,
                "-p",
                base,
                "-m",
                "Apply Scion agent task",
            ],
            false,
        )?)?;
        let reference = format!("refs/scion/attempts/{attempt}");
        run(
            &self.cache,
            "pin task commit",
            &["update-ref", &reference, &head, &"0".repeat(head.len())],
            false,
        )?;
        Ok(head)
    }

    /// Publish one immutable candidate to its dedicated task branch.
    pub fn push_candidate(&self, branch: &str, head: &str) -> Result<(), Error> {
        if !valid_sha(head) || !branch.strip_prefix("scion/").is_some_and(valid_sha) {
            return Err(Error::Invalid("invalid task publication identity"));
        }
        let url = format!("remote.scion-publication.url={}", self.remote);
        let push_url = format!("remote.scion-publication.pushurl={}", self.remote);
        for options in [vec!["--all"], vec!["--push", "--all"]] {
            let mut command = vec!["-c", &url, "-c", &push_url, "remote", "get-url"];
            command.extend(options);
            command.push("scion-publication");
            if line(run(
                &self.cache,
                "validate publication URL",
                &command,
                true,
            )?)? != self.remote
            {
                return Err(Error::Invalid(
                    "Git URL rewrite changes the publication destination",
                ));
            }
        }
        let reference = format!("refs/heads/{branch}");
        let rows = line(run(
            &self.cache,
            "read task branch",
            &["ls-remote", "--refs", &self.remote, &reference],
            true,
        )?)?;
        if rows == format!("{head}\t{reference}") {
            return Ok(());
        }
        if !rows.is_empty() {
            return Err(Error::Invalid(
                "task branch already points to different content",
            ));
        }
        run(
            &self.cache,
            "publish task branch",
            &[
                "-c",
                &url,
                "-c",
                &push_url,
                "push",
                "--no-follow-tags",
                "--recurse-submodules=no",
                &format!("--force-with-lease={reference}:"),
                "scion-publication",
                &format!("{head}:{reference}"),
            ],
            true,
        )?;
        Ok(())
    }
}
