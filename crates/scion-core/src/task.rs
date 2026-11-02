//! Operator-authored task specifications. Saving a task grants no execution permission.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub id: String,
    pub repository: String,
    pub target_branch: String,
    pub goal: String,
    pub allowed_paths: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub owner: String,
    pub max_attempts: u32,
    pub timeout_seconds: u64,
}

impl TaskSpec {
    /// Validate local shape only; repository existence and execution authority are checked later.
    pub fn valid(&self) -> bool {
        let nonempty = |s: &str| !s.trim().is_empty() && !s.chars().any(char::is_control);
        !self.id.is_empty()
            && self.id.len() <= 128
            && nonempty(&self.id)
            && crate::validation::valid_repository(&self.repository)
            && nonempty(&self.target_branch)
            && nonempty(&self.goal)
            && nonempty(&self.owner)
            && self.max_attempts > 0
            && self.timeout_seconds > 0
            && !self.acceptance_criteria.is_empty()
            && self.acceptance_criteria.iter().all(|s| nonempty(s))
            && !self.allowed_paths.is_empty()
            && self.allowed_paths.iter().all(|s| {
                nonempty(s)
                    && !s.contains('\\')
                    && std::path::Path::new(s).components().all(|c| match c {
                        std::path::Component::Normal(name) => name
                            .to_str()
                            .is_some_and(|s| !s.eq_ignore_ascii_case(".git")),
                        _ => false,
                    })
            })
    }
}
