use super::Error;
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const MAX_OUTPUT: u64 = 8 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Capture {
    file: File,
    path: std::path::PathBuf,
}

impl Capture {
    fn new() -> Result<Self, Error> {
        loop {
            let path = std::env::temp_dir().join(format!(
                "scion-git-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => return Ok(Self { file, path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn run(
    dir: &Path,
    args: &[&OsStr],
    network: bool,
) -> Result<(Option<i32>, Vec<u8>), Error> {
    let mut capture = Capture::new()?;
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "0")
        .args([
            "--no-replace-objects",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.ignoreStat=false",
            "-c",
            "submodule.recurse=false",
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(capture.file.try_clone()?);
    if !network {
        command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let started = Instant::now();
    let status = loop {
        let state = child
            .try_wait()
            .and_then(|status| Ok((status, capture.file.metadata()?.len())));
        match state {
            Ok((Some(status), size)) if size <= MAX_OUTPUT => break status,
            Ok((None, size))
                if size <= MAX_OUTPUT && started.elapsed() < Duration::from_secs(90) => {}
            state => {
                #[cfg(unix)]
                let _ = Command::new("/bin/kill")
                    .args(["-KILL", "--", &format!("-{}", child.id())])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let _ = child.kill();
                let _ = child.wait();
                state?;
                return Err(Error::Invalid("Git timed out or exceeded its output limit"));
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    capture.file.seek(SeekFrom::Start(0))?;
    let mut data = Vec::new();
    (&mut capture.file)
        .take(MAX_OUTPUT + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_OUTPUT {
        return Err(Error::Invalid("Git output exceeded its limit"));
    }
    Ok((status.code(), data))
}
