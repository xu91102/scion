use crate::Result;
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_ARCHIVE: u64 = 80 * 1024 * 1024;
const EXECUTE: &str = r#"set -eu
cp -R /input/. /work/
 /usr/bin/timeout --signal=KILL "$1" /scion-agent >/dev/null 2>&1
exec tar -C /work -cf - .
"#;

pub struct Attached {
    child: Child,
    archive: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
}

impl Attached {
    pub fn poll(&mut self) -> Result<Option<(Option<i32>, Vec<u8>)>> {
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        let bytes = self
            .archive
            .take()
            .ok_or("missing Agent output")?
            .join()
            .map_err(|_| "Agent output reader failed")??;
        if bytes.len() as u64 > MAX_ARCHIVE {
            return Err("Agent archive exceeds output limit".into());
        }
        Ok(Some((status.code(), bytes)))
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Docker {
    pub endpoint: String,
}

impl Docker {
    pub fn discover() -> Result<Self> {
        let mut command = Command::new("docker");
        command.args([
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ]);
        Self::local(capture(command)?.trim())
    }

    pub fn local(endpoint: &str) -> Result<Self> {
        let socket = endpoint
            .strip_prefix("unix://")
            .ok_or("Agent requires a local Docker socket")?;
        let path = fs::canonicalize(socket)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            if !path.metadata()?.file_type().is_socket() {
                return Err("Docker endpoint is not a local socket".into());
            }
        }
        Ok(Self {
            endpoint: format!(
                "unix://{}",
                path.to_str().ok_or("Docker socket needs UTF-8")?
            ),
        })
    }

    fn process(&self, args: &[&str]) -> Command {
        let mut command = Command::new("docker");
        command
            .env_remove("DOCKER_CONTEXT")
            .env_remove("DOCKER_HOST")
            .env_remove("DOCKER_TLS_VERIFY")
            .env_remove("DOCKER_CERT_PATH")
            .args(["--host", &self.endpoint])
            .args(args);
        command
    }

    fn command(&self, args: &[&str]) -> Result<String> {
        capture(self.process(args))
    }

    pub fn check_image(&self, image: &str) -> Result<()> {
        let valid = image
            .strip_prefix("sha256:")
            .is_some_and(|sha| sha.len() == 64 && scion_core::validation::valid_sha(sha));
        if !valid {
            return Err("Agent image must be a trusted local sha256 image ID".into());
        }
        let found = self.command(&[
            "image",
            "inspect",
            "--format",
            "{{.Id}}\n{{json (index .Config \"Volumes\")}}",
            image,
        ])?;
        let (id, volumes) = found
            .trim()
            .split_once('\n')
            .ok_or("missing Docker image metadata")?;
        if id != image {
            return Err("Docker image identity changed".into());
        }
        let volumes: Option<std::collections::BTreeMap<String, serde_json::Value>> =
            serde_json::from_str(volumes)?;
        if volumes.is_some_and(|volumes| !volumes.is_empty()) {
            return Err("Agent image declares writable Docker volumes".into());
        }
        Ok(())
    }

    pub fn create(
        &self,
        key: &str,
        image: &str,
        root: &Path,
        auth: Option<&Path>,
        seconds: u64,
    ) -> Result<()> {
        let work = root.join("work");
        #[cfg(unix)]
        let user = {
            use std::os::unix::fs::MetadataExt;
            let meta = work.metadata()?;
            format!("{}:{}", meta.uid(), meta.gid())
        };
        #[cfg(not(unix))]
        let user = return Err("Agent containers require a Unix host".into());
        let name = name(key);
        let label = format!("io.scion.attempt={key}");
        let workspace = mount(&work, "/input", true)?;
        let prompt = mount(&root.join("task.json"), "/run/scion-task", true)?;
        let credentials = auth
            .map(|path| mount(path, "/run/scion-auth", true))
            .transpose()?;
        let mut args = vec![
            "create",
            "--name",
            &name,
            "--label",
            &label,
            "--pull=never",
            "--user",
            &user,
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--network=bridge",
            "--read-only",
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,size=128m,mode=1777",
            "--memory=2g",
            "--cpus=2",
            "--pids-limit=128",
            "--log-driver=none",
            "--workdir=/work",
            "--entrypoint=/bin/sh",
            "--tmpfs",
            "/work:rw,nosuid,nodev,size=64m,nr_inodes=20000,mode=1777",
            "--mount",
            &workspace,
            "--mount",
            &prompt,
        ];
        if let Some(auth) = credentials.as_deref() {
            args.extend(["--mount", auth]);
        }
        let limit = format!("{seconds}s");
        args.extend([image, "-c", EXECUTE, "scion-agent", &limit]);
        self.command(&args)?;
        Ok(())
    }

    fn owned(&self, key: &str) -> Result<Option<String>> {
        let filter = format!("name=^/{}$", name(key));
        let data = self.command(&["container", "ls", "-aq", "--no-trunc", "--filter", &filter])?;
        let id = data.trim();
        if id.is_empty() {
            return Ok(None);
        }
        if id.len() != 64 || !scion_core::validation::valid_sha(id) {
            return Err("ambiguous Docker container identity".into());
        }
        let label = self.command(&[
            "container",
            "inspect",
            "--format",
            "{{index .Config.Labels \"io.scion.attempt\"}}",
            id,
        ])?;
        if label.trim() != key {
            return Err("Docker container ownership mismatch".into());
        }
        Ok(Some(id.to_owned()))
    }

    pub fn start(&self, key: &str) -> Result<Attached> {
        let id = self.owned(key)?.ok_or("Agent container disappeared")?;
        let mut child = self
            .process(&["start", "-a", &id])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let output = child.stdout.take().ok_or("missing Agent output pipe")?;
        let archive = thread::spawn(move || {
            let mut bytes = Vec::new();
            output.take(MAX_ARCHIVE + 1).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        Ok(Attached {
            child,
            archive: Some(archive),
        })
    }

    pub fn remove(&self, key: &str) -> Result<()> {
        if let Some(id) = self.owned(key)? {
            self.command(&["container", "rm", "-f", "-v", &id])?;
        }
        if self.owned(key)?.is_some() {
            return Err("Agent container termination was not confirmed".into());
        }
        Ok(())
    }
}

fn name(key: &str) -> String {
    format!("scion-agent-{key}")
}

fn mount(source: &Path, destination: &str, read_only: bool) -> Result<String> {
    let path = source.to_str().ok_or("Docker mount requires UTF-8 path")?;
    if !source.is_absolute() || path.contains([',', '"', '\n', '\r']) {
        return Err("unsupported Docker mount path".into());
    }
    let mode = if read_only { ",readonly" } else { "" };
    Ok(format!("type=bind,src={path},dst={destination}{mode}"))
}

/// Capture bounded trusted CLI metadata. Model stdout and credential values never reach this path.
pub(super) fn capture(mut command: Command) -> Result<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let output = child.stdout.take().ok_or("missing CLI stdout")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        output.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(15) => {
                thread::sleep(Duration::from_millis(25))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("external CLI operation timed out or failed".into());
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "external CLI output reader failed")??;
    if !status.success() || bytes.len() > 1024 * 1024 {
        return Err(format!(
            "external CLI operation failed with exit {:?}",
            status.code()
        )
        .into());
    }
    Ok(String::from_utf8(bytes)?)
}
