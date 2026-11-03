//! Read-only GitHub observations. Only GET requests to api.github.com are exposed.

use reqwest::{
    blocking::Client,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use std::time::Duration;

mod observe;
mod summary;
mod transport;

#[derive(Debug, thiserror::Error)]
pub enum GitHubError {
    #[error("invalid repository, PR number or credential format")]
    InvalidInput,
    #[error("GitHub request failed; check network or credentials")]
    Transport,
    #[error("GitHub GET returned HTTP {0}")]
    Http(u16),
    #[error("GitHub returned invalid or inconsistent observation data")]
    InvalidResponse,
    #[error("GitHub result exceeds the bounded pagination or response limit")]
    Limit,
}

/// Authenticated read-only client; credentials are never included in reports or Debug output.
pub struct GitHubClient {
    client: Client,
    base: reqwest::Url,
}

impl GitHubClient {
    pub fn new(token: Option<&str>) -> Result<Self, GitHubError> {
        Self::with_base("https://api.github.com/", token)
    }
    fn with_base(base: &str, token: Option<&str>) -> Result<Self, GitHubError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "accept",
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static("2026-03-10"),
        );
        if let Some(token) = token.filter(|s| !s.is_empty()) {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| GitHubError::InvalidInput)?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        let client = Client::builder()
            .default_headers(headers)
            .user_agent("scion-m1/0.0.1")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| GitHubError::Transport)?;
        Ok(Self {
            client,
            base: reqwest::Url::parse(base).map_err(|_| GitHubError::InvalidInput)?,
        })
    }
}

#[cfg(test)]
mod tests;
