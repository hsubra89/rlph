use crate::error::Result;

#[derive(Debug)]
pub struct SubmitResult {
    pub url: String,
}

pub trait SubmissionBackend {
    /// Submit a branch as a PR or diff. Returns the URL of the created PR/diff.
    fn submit(&self, branch: &str, base: &str, title: &str, body: &str) -> Result<SubmitResult>;
}
