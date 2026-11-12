use crate::Result;
use scion_core::workspace::{valid_path, within};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::Path,
};

#[derive(PartialEq, Eq)]
pub(super) struct Entry {
    hash: [u8; 32],
    executable: bool,
}

pub(super) type Manifest = BTreeMap<String, Entry>;

pub(super) fn manifest(root: &Path) -> Result<Manifest> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0;
    let mut remaining = 64 * 1024 * 1024;
    while let Some(directory) = pending.pop() {
        for child in fs::read_dir(directory)? {
            let child = child?;
            let path = child.path();
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("snapshot path requires UTF-8")?;
            count += 1;
            if count > 20_000 || relative.split('/').count() > 64 || !valid_path(relative) {
                return Err("snapshot has unsupported paths or too many entries".into());
            }
            let meta = path.symlink_metadata()?;
            if meta.is_dir() {
                pending.push(path);
                continue;
            }
            if !meta.is_file() || meta.len() > remaining {
                return Err("snapshot contains a non-regular file or exceeds 64 MiB".into());
            }
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::MetadataExt;
                if meta.nlink() != 1 {
                    return Err("snapshot contains a hard link".into());
                }
                meta.mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            let mut file = fs::File::open(&path)?.take(remaining + 1);
            let mut hash = Sha256::new();
            let mut buffer = [0; 8192];
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                remaining = remaining
                    .checked_sub(n as u64)
                    .ok_or("snapshot exceeds 64 MiB")?;
                hash.write_all(&buffer[..n])?;
            }
            files.insert(
                relative.to_owned(),
                Entry {
                    hash: hash.finalize().into(),
                    executable,
                },
            );
        }
    }
    Ok(files)
}

pub(super) fn changes(
    before: &Manifest,
    after: &Manifest,
    allowed: &[String],
) -> Result<Vec<(String, Option<bool>)>> {
    let names: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let mut changes = Vec::new();
    for path in names {
        if before.get(path) == after.get(path) {
            continue;
        }
        if !allowed.iter().any(|prefix| within(path, prefix)) || protected(path) {
            return Err(
                "Agent changed paths outside scope or protected Git/CI configuration".into(),
            );
        }
        changes.push((path.clone(), after.get(path).map(|entry| entry.executable)));
    }
    if changes.is_empty() {
        return Err("Agent produced no file changes".into());
    }
    Ok(changes)
}

fn protected(path: &str) -> bool {
    path.split('/').any(|part| {
        [".github", ".gitmodules", ".gitattributes"]
            .iter()
            .any(|name| part.eq_ignore_ascii_case(name))
    })
}

pub(super) fn secrets(auth: Option<&Path>) -> Result<Vec<Vec<u8>>> {
    let Some(auth) = auth else {
        return Ok(vec![]);
    };
    let mut data = Vec::new();
    fs::File::open(auth)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    if data.len() > 1024 * 1024 {
        return Err("provider credential file exceeds 1 MiB".into());
    }
    let mut secrets = Vec::new();
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) {
        let mut pending = vec![&value];
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::String(text) if text.len() >= 8 => {
                    secrets.push(text.as_bytes().to_vec())
                }
                serde_json::Value::Array(items) => pending.extend(items),
                serde_json::Value::Object(map) => pending.extend(map.values()),
                _ => {}
            }
        }
    }
    if data.len() >= 8 {
        secrets.push(data);
    }
    secrets.sort();
    secrets.dedup();
    if secrets.len() > 64 {
        return Err("provider credential file has too many secret fields".into());
    }
    Ok(secrets)
}

pub(super) fn extract(archive: &[u8], output: &Path, secrets: &[Vec<u8>]) -> Result<()> {
    let contains_secret = |bytes: &[u8]| {
        secrets.iter().any(|secret| {
            !secret.is_empty() && bytes.windows(secret.len()).any(|window| window == secret)
        })
    };
    if archive.len() < 1024 {
        return Err("Agent did not return a complete snapshot archive".into());
    }
    let mut archive = tar::Archive::new(archive);
    let mut remaining = 64 * 1024 * 1024u64;
    for (number, entry) in archive.entries()?.enumerate() {
        let mut entry = entry?;
        let path = entry.path()?;
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::Normal(name) => {
                    let name = name.to_str().ok_or("archive path requires UTF-8")?;
                    if !valid_path(name) {
                        return Err("unsafe Agent archive path".into());
                    }
                    parts.push(name);
                }
                _ => return Err("unsafe Agent archive path".into()),
            }
        }
        if number >= 20_000 || parts.len() > 64 {
            return Err("Agent archive has too many entries".into());
        }
        let name = parts.join("/");
        if contains_secret(name.as_bytes()) {
            return Err("Agent output contains model credentials".into());
        }
        if entry.header().entry_type().is_dir() {
            continue;
        }
        if name.is_empty() || !entry.header().entry_type().is_file() || entry.size() > remaining {
            return Err("Agent archive has a special file or exceeds its limit".into());
        }
        let mode = entry.header().mode()?;
        let mut data = Vec::new();
        (&mut entry).take(remaining + 1).read_to_end(&mut data)?;
        remaining = remaining
            .checked_sub(data.len() as u64)
            .ok_or("Agent archive exceeds its limit")?;
        if contains_secret(&data) {
            return Err("Agent output contains model credentials".into());
        }
        let file = output.join(&name);
        fs::create_dir_all(file.parent().ok_or("invalid archive path")?)?;
        let mut handle = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file)?;
        handle.write_all(&data)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            handle.set_permissions(fs::Permissions::from_mode(if mode & 0o111 == 0 {
                0o644
            } else {
                0o755
            }))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_candidate_rejects_ci_and_sibling_paths() {
        let before = Manifest::new();
        let entry = || Entry {
            hash: [1; 32],
            executable: false,
        };
        let after = Manifest::from([("src/a/file".into(), entry())]);
        assert_eq!(
            changes(&before, &after, &["src/a".into()]).unwrap()[0].0,
            "src/a/file"
        );
        assert!(changes(&before, &after, &["src/ab".into()]).is_err());
        let after = Manifest::from([(".GitHub/workflows/ci.yml".into(), entry())]);
        assert!(changes(&before, &after, &[".GitHub".into()]).is_err());
    }
}
