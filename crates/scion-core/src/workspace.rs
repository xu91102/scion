//! Local workspace coordination contracts.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub repository: String,
    pub source_checkout: PathBuf,
    pub state_root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub task: String,
    pub repository: String,
    pub target: String,
    pub base: String,
    pub branch: String,
    pub path: PathBuf,
    pub allowed: Vec<String>,
    pub sparse: Vec<String>,
    pub config_digest: String,
    pub spec_digest: String,
    pub phase: Phase,
    pub revision: i64,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Reserved,
    Preparing,
    Ready,
    Released,
}

pub fn valid_path(path: &str) -> bool {
    !path.contains('\\')
        && !path.contains(':')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.eq_ignore_ascii_case(".git")
        })
}

pub fn within(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_boundaries_are_components() {
        assert!(within("src/a/file", "src/a"));
        assert!(!within("src/ab/file", "src/a"));
        for path in [
            "",
            "/src",
            "src/../other",
            "src/.Git/config",
            "src//a",
            "C:/src",
        ] {
            assert!(!valid_path(path), "{path}");
        }
    }
}
