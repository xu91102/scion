use super::{GitHubClient, GitHubError};
use serde_json::Value;
use std::io::Read;

const MAX_RESPONSE: u64 = 4 * 1024 * 1024;
const MAX_PAGES: usize = 20;

impl GitHubClient {
    pub(crate) fn request(
        &self,
        path: &[&str],
        page: Option<usize>,
    ) -> Result<(u16, Value), GitHubError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| GitHubError::InvalidInput)?
            .clear()
            .extend(path.iter().copied());
        if let Some(page) = page {
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page.to_string());
            if path.last() == Some(&"check-runs") {
                url.query_pairs_mut().append_pair("filter", "latest");
            }
        }
        let response = self
            .client
            .get(url)
            .send()
            .map_err(|_| GitHubError::Transport)?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        response
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| GitHubError::Transport)?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err(GitHubError::Limit);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| GitHubError::InvalidResponse)?;
        Ok((status, value))
    }
    pub(crate) fn value(&self, path: &[&str]) -> Result<Value, GitHubError> {
        let (status, value) = self.request(path, None)?;
        if status != 200 {
            return Err(GitHubError::Http(status));
        }
        Ok(value)
    }
    pub(crate) fn pages(
        &self,
        path: &[&str],
        field: Option<&str>,
    ) -> Result<Vec<Value>, GitHubError> {
        let mut result = Vec::new();
        for page in 1..=MAX_PAGES {
            let (status, value) = self.request(path, Some(page))?;
            if status != 200 {
                return Err(GitHubError::Http(status));
            }
            let values = field
                .map_or(&value, |key| &value[key])
                .as_array()
                .ok_or(GitHubError::InvalidResponse)?;
            result.extend(values.iter().cloned());
            if values.len() < 100 {
                return Ok(result);
            }
        }
        Err(GitHubError::Limit)
    }
}

pub(crate) fn text(value: &Value) -> Result<String, GitHubError> {
    value
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or(GitHubError::InvalidResponse)
}
pub(crate) fn number(value: &Value) -> Result<u64, GitHubError> {
    value
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or(GitHubError::InvalidResponse)
}
