//! Canonical syntax shared by the offline evaluator and read-only adapters.

/// Validate a standard github.com owner/repository identifier.
pub fn valid_repository(repository: &str) -> bool {
    let Some((owner, name)) = repository.split_once('/') else {
        return false;
    };
    let valid_owner = !owner.is_empty()
        && owner.len() <= 39
        && owner
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && owner
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && !owner.contains("--")
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    valid_owner
        && !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// A full lowercase Git SHA-1 or SHA-256 identifier.
pub fn valid_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64)
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
