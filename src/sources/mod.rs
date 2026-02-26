pub mod github;
pub mod linear;

use serde::Serialize;

use std::collections::HashSet;

use crate::deps;
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

/// A task or group of related tasks (parent + sub-issues).
#[derive(Debug, Clone, Serialize)]
pub enum TaskGroup {
    Standalone(Task),
    Group { parent: Task, sub_issues: Vec<Task> },
}

impl TaskGroup {
    pub fn parent(&self) -> &Task {
        match self {
            TaskGroup::Standalone(task) => task,
            TaskGroup::Group { parent, .. } => parent,
        }
    }

    pub fn all_sub_issue_ids(&self) -> Vec<String> {
        match self {
            TaskGroup::Standalone(task) => vec![task.id.clone()],
            TaskGroup::Group { sub_issues, .. } => {
                sub_issues.iter().map(|t| t.id.clone()).collect()
            }
        }
    }

    pub fn is_complete(&self, done_ids: &HashSet<u64>) -> bool {
        match self {
            TaskGroup::Standalone(task) => task
                .id
                .parse::<u64>()
                .ok()
                .is_some_and(|id| done_ids.contains(&id)),
            TaskGroup::Group { sub_issues, .. } => sub_issues.iter().all(|task| {
                task.id
                    .parse::<u64>()
                    .ok()
                    .is_some_and(|id| done_ids.contains(&id))
            }),
        }
    }

    /// Return the next sub-issue whose intra-group dependencies are satisfied.
    /// Sub-issues are stored in topological order; this returns the first
    /// not-yet-done one whose intra-group deps are all in `done_ids`.
    pub fn next_eligible_sub_issue(&self, done_ids: &HashSet<u64>) -> Option<&Task> {
        match self {
            TaskGroup::Standalone(task) => {
                let id = task.id.parse::<u64>().ok()?;
                if done_ids.contains(&id) {
                    None
                } else {
                    Some(task)
                }
            }
            TaskGroup::Group { sub_issues, .. } => sub_issues.iter().find(|task| {
                let id = match task.id.parse::<u64>() {
                    Ok(n) => n,
                    Err(_) => return false,
                };
                if done_ids.contains(&id) {
                    return false;
                }
                let task_deps = deps::parse_dependencies(&task.body);
                task_deps.iter().all(|d| done_ids.contains(d))
            }),
        }
    }
}

pub trait TaskSource {
    /// Fetch tasks matching the label filter, excluding blocked ones.
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>>;

    /// Fetch tasks grouped by parent/sub-issue relationships.
    /// Default wraps each task as `TaskGroup::Standalone`.
    fn fetch_eligible_task_groups(&self) -> Result<Vec<TaskGroup>> {
        Ok(self
            .fetch_eligible_tasks()?
            .into_iter()
            .map(TaskGroup::Standalone)
            .collect())
    }

    /// Mark a task as in-progress in the remote system.
    fn mark_in_progress(&self, task_id: &str) -> Result<()>;

    /// Mark a task as in-review in the remote system.
    fn mark_in_review(&self, task_id: &str) -> Result<()>;

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

    fn fetch_eligible_task_groups(&self) -> Result<Vec<TaskGroup>> {
        match self {
            AnySource::GitHub(s) => s.fetch_eligible_task_groups(),
            AnySource::Linear(s) => s.fetch_eligible_task_groups(),
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

    // --- TaskGroup tests ---

    fn make_task(id: u64, body: &str) -> Task {
        Task {
            id: id.to_string(),
            title: format!("Task {id}"),
            body: body.to_string(),
            labels: vec![],
            url: String::new(),
            priority: None,
        }
    }

    #[test]
    fn test_standalone_parent() {
        let group = TaskGroup::Standalone(make_task(1, "body"));
        assert_eq!(group.parent().id, "1");
    }

    #[test]
    fn test_standalone_all_sub_issue_ids() {
        let group = TaskGroup::Standalone(make_task(5, ""));
        assert_eq!(group.all_sub_issue_ids(), vec!["5"]);
    }

    #[test]
    fn test_standalone_is_complete() {
        let group = TaskGroup::Standalone(make_task(5, ""));
        let empty: HashSet<u64> = HashSet::new();
        assert!(!group.is_complete(&empty));
        let done: HashSet<u64> = [5].into();
        assert!(group.is_complete(&done));
    }

    #[test]
    fn test_standalone_next_eligible() {
        let group = TaskGroup::Standalone(make_task(5, ""));
        let empty: HashSet<u64> = HashSet::new();
        assert_eq!(group.next_eligible_sub_issue(&empty).unwrap().id, "5");
        let done: HashSet<u64> = [5].into();
        assert!(group.next_eligible_sub_issue(&done).is_none());
    }

    #[test]
    fn test_group_parent() {
        let group = TaskGroup::Group {
            parent: make_task(10, "parent"),
            sub_issues: vec![make_task(11, ""), make_task(12, "")],
        };
        assert_eq!(group.parent().id, "10");
    }

    #[test]
    fn test_group_all_sub_issue_ids() {
        let group = TaskGroup::Group {
            parent: make_task(10, ""),
            sub_issues: vec![make_task(11, ""), make_task(12, ""), make_task(13, "")],
        };
        assert_eq!(group.all_sub_issue_ids(), vec!["11", "12", "13"]);
    }

    #[test]
    fn test_group_is_complete() {
        let group = TaskGroup::Group {
            parent: make_task(10, ""),
            sub_issues: vec![make_task(11, ""), make_task(12, "")],
        };
        let empty: HashSet<u64> = HashSet::new();
        assert!(!group.is_complete(&empty));
        let partial: HashSet<u64> = [11].into();
        assert!(!group.is_complete(&partial));
        let all: HashSet<u64> = [11, 12].into();
        assert!(group.is_complete(&all));
    }

    #[test]
    fn test_group_next_eligible_no_deps() {
        let group = TaskGroup::Group {
            parent: make_task(10, ""),
            sub_issues: vec![make_task(11, ""), make_task(12, "")],
        };
        let empty: HashSet<u64> = HashSet::new();
        // First eligible is the first sub-issue
        assert_eq!(group.next_eligible_sub_issue(&empty).unwrap().id, "11");
        // After completing 11, next is 12
        let done: HashSet<u64> = [11].into();
        assert_eq!(group.next_eligible_sub_issue(&done).unwrap().id, "12");
        // All done → none
        let all: HashSet<u64> = [11, 12].into();
        assert!(group.next_eligible_sub_issue(&all).is_none());
    }

    #[test]
    fn test_group_next_eligible_with_deps() {
        // Sub-issue 12 depends on 11 (intra-group)
        let group = TaskGroup::Group {
            parent: make_task(10, ""),
            sub_issues: vec![make_task(11, "no deps"), make_task(12, "Blocked by #11")],
        };
        let empty: HashSet<u64> = HashSet::new();
        // 11 is eligible, 12 blocked by 11
        assert_eq!(group.next_eligible_sub_issue(&empty).unwrap().id, "11");
        // After 11 done, 12 is eligible
        let done: HashSet<u64> = [11].into();
        assert_eq!(group.next_eligible_sub_issue(&done).unwrap().id, "12");
    }

    #[test]
    fn test_group_next_eligible_external_dep_blocks() {
        // Sub-issue 12 depends on #99 (external, not in group) — blocks until resolved
        let group = TaskGroup::Group {
            parent: make_task(10, ""),
            sub_issues: vec![make_task(11, ""), make_task(12, "Blocked by #99")],
        };
        let empty: HashSet<u64> = HashSet::new();
        // 11 is eligible, but 12 is blocked by external #99
        assert_eq!(group.next_eligible_sub_issue(&empty).unwrap().id, "11");
        // After completing 11, 12 is still blocked by #99
        let done_11: HashSet<u64> = [11].into();
        assert!(
            group.next_eligible_sub_issue(&done_11).is_none(),
            "sub-issue 12 should be blocked by external dep #99"
        );
        // Once #99 is also in done_ids, 12 becomes eligible
        let done_11_99: HashSet<u64> = [11, 99].into();
        assert_eq!(group.next_eligible_sub_issue(&done_11_99).unwrap().id, "12");
    }

    #[test]
    fn test_group_next_eligible_diamond_deps() {
        // Diamond: 11 → {12, 13} → 14
        // 12 and 13 depend on 11; 14 depends on 12 and 13
        let group = TaskGroup::Group {
            parent: make_task(10, ""),
            sub_issues: vec![
                make_task(11, ""),
                make_task(12, "Blocked by #11"),
                make_task(13, "Blocked by #11"),
                make_task(14, "blockedBy: [12, 13]"),
            ],
        };
        let empty: HashSet<u64> = HashSet::new();
        assert_eq!(group.next_eligible_sub_issue(&empty).unwrap().id, "11");

        let done11: HashSet<u64> = [11].into();
        let next = group.next_eligible_sub_issue(&done11).unwrap();
        assert!(next.id == "12" || next.id == "13");

        let done12_13: HashSet<u64> = [11, 12, 13].into();
        assert_eq!(group.next_eligible_sub_issue(&done12_13).unwrap().id, "14");
    }
}
