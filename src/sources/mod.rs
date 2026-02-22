pub mod github;

use serde::Serialize;

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
    fn mark_done(&self, task_id: &str) -> Result<()>;

    /// Get full details for a task.
    fn get_task_details(&self, task_id: &str) -> Result<Task>;
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
