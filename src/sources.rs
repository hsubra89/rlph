use crate::error::Result;

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub url: String,
}

pub trait TaskSource {
    /// Fetch tasks matching the label filter, excluding blocked ones.
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>>;

    /// Mark a task as in-progress in the remote system.
    fn mark_in_progress(&self, task_id: &str) -> Result<()>;

    /// Mark a task as done in the remote system.
    fn mark_done(&self, task_id: &str) -> Result<()>;

    /// Get full details for a task.
    fn get_task_details(&self, task_id: &str) -> Result<Task>;
}
