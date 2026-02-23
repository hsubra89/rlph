pub mod github;
pub mod linear;

use serde::Serialize;

use std::collections::HashSet;

use crate::error::Result;

/// Task priority (1 = highest, 9 = lowest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Priority(pub u8);

impl Priority {
    /// Parse priority from a label string.
    /// Recognizes: p1-p9, priority-high, priority-medium, priority-low.
    pub fn from_label(label: &str) -> Option<Self> {
        let lower = label.to_lowercase();
        match lower.as_str() {
            "priority-high" => Some(Priority(1)),
            "priority-medium" => Some(Priority(5)),
            "priority-low" => Some(Priority(9)),
            s if s.len() == 2 && s.starts_with('p') => s[1..]
                .parse::<u8>()
                .ok()
                .filter(|&n| (1..=9).contains(&n))
                .map(Priority),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub url: String,
    pub priority: Option<Priority>,
}

pub trait TaskSource {
    /// Fetch tasks matching the label filter, excluding blocked ones.
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>>;

    /// Mark a task as in-progress in the remote system.
    fn mark_in_progress(&self, task_id: &str) -> Result<()>;

    /// Mark a task as in-review in the remote system.
    fn mark_in_review(&self, task_id: &str) -> Result<()>;

    /// Mark a task as done in the remote system.
    ///
    /// Currently unused in the happy path — GitHub auto-closes issues when the
    /// PR containing "Resolves #N" is merged. Kept for manual / fallback use.
    fn mark_done(&self, task_id: &str) -> Result<()>;

    /// Get full details for a task.
    fn get_task_details(&self, task_id: &str) -> Result<Task>;

    /// Fetch IDs of closed/done tasks (used for dependency resolution).
    fn fetch_closed_task_ids(&self) -> Result<HashSet<u64>>;
}

pub enum AnySource {
    GitHub(github::GitHubSource),
    Linear(linear::LinearSource),
}

impl TaskSource for AnySource {
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>> {
        match self {
            AnySource::GitHub(s) => s.fetch_eligible_tasks(),
            AnySource::Linear(s) => s.fetch_eligible_tasks(),
        }
    }

    fn mark_in_progress(&self, task_id: &str) -> Result<()> {
        match self {
            AnySource::GitHub(s) => s.mark_in_progress(task_id),
            AnySource::Linear(s) => s.mark_in_progress(task_id),
        }
    }

    fn mark_in_review(&self, task_id: &str) -> Result<()> {
        match self {
            AnySource::GitHub(s) => s.mark_in_review(task_id),
            AnySource::Linear(s) => s.mark_in_review(task_id),
        }
    }

    fn mark_done(&self, task_id: &str) -> Result<()> {
        match self {
            AnySource::GitHub(s) => s.mark_done(task_id),
            AnySource::Linear(s) => s.mark_done(task_id),
        }
    }

    fn get_task_details(&self, task_id: &str) -> Result<Task> {
        match self {
            AnySource::GitHub(s) => s.get_task_details(task_id),
            AnySource::Linear(s) => s.get_task_details(task_id),
        }
    }

    fn fetch_closed_task_ids(&self) -> Result<HashSet<u64>> {
        match self {
            AnySource::GitHub(s) => s.fetch_closed_task_ids(),
            AnySource::Linear(s) => s.fetch_closed_task_ids(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_from_numeric_labels() {
        assert_eq!(Priority::from_label("p1"), Some(Priority(1)));
        assert_eq!(Priority::from_label("p5"), Some(Priority(5)));
        assert_eq!(Priority::from_label("p9"), Some(Priority(9)));
    }

    #[test]
    fn test_priority_from_named_labels() {
        assert_eq!(Priority::from_label("priority-high"), Some(Priority(1)));
        assert_eq!(Priority::from_label("priority-medium"), Some(Priority(5)));
        assert_eq!(Priority::from_label("priority-low"), Some(Priority(9)));
    }

    #[test]
    fn test_priority_case_insensitive() {
        assert_eq!(Priority::from_label("P1"), Some(Priority(1)));
        assert_eq!(Priority::from_label("Priority-High"), Some(Priority(1)));
        assert_eq!(Priority::from_label("PRIORITY-LOW"), Some(Priority(9)));
    }

    #[test]
    fn test_priority_invalid() {
        assert_eq!(Priority::from_label("p0"), None);
        assert_eq!(Priority::from_label("p10"), None);
        assert_eq!(Priority::from_label("bug"), None);
        assert_eq!(Priority::from_label(""), None);
    }
}
