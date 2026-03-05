use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::error::Result;
use crate::reference_rewriter::rewrite_issue_urls;
use crate::sources::{Task, TaskSource};
use crate::worktree::WorktreeManager;

const MAX_REFERENCE_DEPTH: usize = 4;

static ISSUE_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/(?:issues|pull)/([0-9]+)|https://linear\.app/[A-Za-z0-9_-]+/issue/([A-Za-z0-9-]+)",
    )
    .expect("issue URL regex compiles")
});

static LINEAR_NUMERIC_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]+-([0-9]+)$").expect("linear suffix regex compiles"));

#[derive(Debug, Clone)]
pub struct PlanDirectory {
    pub path: PathBuf,
    pub files: Vec<PathBuf>,
}

pub fn sync_to_local<S: TaskSource>(
    source: &S,
    issue_id: &str,
    plans_dir: &Path,
) -> Result<PlanDirectory> {
    let main_task = source.get_task_details(issue_id)?;
    let mut tasks_by_id: HashMap<String, Task> = HashMap::new();
    tasks_by_id.insert(main_task.id.clone(), main_task.clone());

    for sub_issue in source.fetch_sub_issues(&main_task.id)? {
        let detailed_sub_issue = source.get_task_details(&sub_issue.id)?;
        tasks_by_id
            .entry(detailed_sub_issue.id.clone())
            .or_insert(detailed_sub_issue);
    }

    let mut queued_for_scan: VecDeque<(String, usize)> =
        tasks_by_id.keys().cloned().map(|id| (id, 0)).collect();
    let mut scanned: HashSet<String> = HashSet::new();

    while let Some((task_id, depth)) = queued_for_scan.pop_front() {
        if !scanned.insert(task_id.clone()) {
            continue;
        }

        let Some(task) = tasks_by_id.get(&task_id) else {
            continue;
        };

        if depth >= MAX_REFERENCE_DEPTH {
            continue;
        }

        let refs = extract_referenced_task_ids(&task.body);
        for ref_id in refs {
            if tasks_by_id.contains_key(&ref_id) {
                continue;
            }

            let referenced = source.get_task_details(&ref_id)?;
            tasks_by_id.insert(referenced.id.clone(), referenced.clone());
            queued_for_scan.push_back((referenced.id, depth + 1));
        }
    }

    let slug = plan_slug(&main_task);
    let plan_path = plans_dir.join(slug);
    fs::create_dir_all(&plan_path)?;

    let mut local_ids = HashSet::new();
    for task in tasks_by_id.values() {
        local_ids.insert(task.id.clone());
    }

    for task in tasks_by_id.values() {
        let content = format!("# {}\n\n{}\n", task.title, task.body);
        fs::write(plan_path.join(issue_filename(&task.id)), content)?;
    }

    for file in list_plan_files(&plan_path)? {
        let content = fs::read_to_string(&file)?;
        let rewritten = rewrite_issue_urls(&content, &local_ids);
        fs::write(&file, rewritten)?;
    }

    let files = list_plan_files(&plan_path)?;
    Ok(PlanDirectory {
        path: plan_path,
        files,
    })
}

pub fn list_plan_files(plan_dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(plan_dir)?;
    let mut files = Vec::new();

    for entry in entries {
        let path = entry?.path();
        if path.is_file() {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

fn issue_filename(task_id: &str) -> String {
    format!("{}.md", task_id)
}

fn plan_slug(main_task: &Task) -> String {
    let slug = WorktreeManager::slugify(&main_task.title);
    if !slug.is_empty() {
        return slug;
    }

    let mut cleaned = String::new();
    for c in main_task.id.chars() {
        if c.is_ascii_alphanumeric() {
            cleaned.push(c.to_ascii_lowercase());
        } else if !cleaned.ends_with('-') {
            cleaned.push('-');
        }
    }

    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() {
        "gh-task".to_string()
    } else {
        format!("gh-{cleaned}")
    }
}

fn extract_referenced_task_ids(markdown: &str) -> HashSet<String> {
    let mut ids = HashSet::new();

    for captures in ISSUE_URL_RE.captures_iter(markdown) {
        if let Some(github_id) = captures.get(1) {
            ids.insert(github_id.as_str().to_string());
            continue;
        }

        let Some(linear_id) = captures.get(2).map(|m| m.as_str()) else {
            continue;
        };

        if let Some(num) = LINEAR_NUMERIC_SUFFIX_RE
            .captures(linear_id)
            .and_then(|caps| caps.get(1))
        {
            ids.insert(num.as_str().to_string());
        } else {
            ids.insert(linear_id.to_string());
        }
    }

    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::collections::BTreeSet;

    use tempfile::TempDir;

    use crate::error::Error;
    use crate::ids::IssueNumber;

    struct MockTaskSource {
        tasks: HashMap<String, Task>,
        sub_issues: HashMap<String, Vec<Task>>,
        detail_calls: RefCell<Vec<String>>,
    }

    impl MockTaskSource {
        fn new(tasks: HashMap<String, Task>, sub_issues: HashMap<String, Vec<Task>>) -> Self {
            Self {
                tasks,
                sub_issues,
                detail_calls: RefCell::new(Vec::new()),
            }
        }

        fn detail_calls(&self) -> Vec<String> {
            self.detail_calls.borrow().clone()
        }
    }

    impl TaskSource for MockTaskSource {
        fn fetch_eligible_tasks(&self) -> Result<Vec<Task>> {
            Ok(Vec::new())
        }

        fn mark_in_progress(&self, _task_id: &str) -> Result<()> {
            Ok(())
        }

        fn mark_in_review(&self, _task_id: &str) -> Result<()> {
            Ok(())
        }

        fn get_task_details(&self, task_id: &str) -> Result<Task> {
            self.detail_calls.borrow_mut().push(task_id.to_string());
            self.tasks.get(task_id).cloned().ok_or_else(|| {
                Error::TaskSource(format!("missing mock task details for task id: {task_id}"))
            })
        }

        fn fetch_sub_issues(&self, task_id: &str) -> Result<Vec<Task>> {
            Ok(self.sub_issues.get(task_id).cloned().unwrap_or_default())
        }

        fn fetch_closed_task_ids(&self) -> Result<HashSet<IssueNumber>> {
            Ok(HashSet::new())
        }
    }

    fn task(id: &str, title: &str, body: &str) -> Task {
        Task {
            id: id.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            labels: vec![],
            url: format!("https://example.test/tasks/{id}"),
            priority: None,
        }
    }

    fn file_names(paths: &[PathBuf]) -> BTreeSet<String> {
        paths
            .iter()
            .map(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn sync_writes_main_sub_and_referenced_issues() {
        let tmp = TempDir::new().unwrap();

        let mut tasks = HashMap::new();
        tasks.insert(
            "42".to_string(),
            task(
                "42",
                "Fix Auth Bug",
                "Depends on https://github.com/org/repo/issues/50",
            ),
        );
        tasks.insert(
            "45".to_string(),
            task(
                "45",
                "Follow-up",
                "See https://github.com/org/repo/issues/63 for context",
            ),
        );
        tasks.insert("50".to_string(), task("50", "Root cause", "Details"));
        tasks.insert("63".to_string(), task("63", "Logging", "Details"));

        let mut sub_issues = HashMap::new();
        sub_issues.insert("42".to_string(), vec![task("45", "Follow-up", "")]);

        let source = MockTaskSource::new(tasks, sub_issues);
        let plan = sync_to_local(&source, "42", tmp.path()).unwrap();

        assert!(plan.path.ends_with("fix-auth-bug"));
        assert_eq!(
            file_names(&plan.files),
            BTreeSet::from([
                "42.md".to_string(),
                "45.md".to_string(),
                "50.md".to_string(),
                "63.md".to_string()
            ])
        );

        let main_file = fs::read_to_string(plan.path.join("42.md")).unwrap();
        assert!(main_file.contains("[#50](./50.md)"));
        let sub_file = fs::read_to_string(plan.path.join("45.md")).unwrap();
        assert!(sub_file.contains("[#63](./63.md)"));
    }

    #[test]
    fn sync_limits_reference_crawl_depth_to_four() {
        let tmp = TempDir::new().unwrap();

        let mut tasks = HashMap::new();
        for i in 1..=7 {
            let body = if i < 7 {
                format!("https://github.com/org/repo/issues/{}", i + 1)
            } else {
                String::new()
            };
            tasks.insert(
                i.to_string(),
                task(&i.to_string(), &format!("Issue {i}"), &body),
            );
        }

        let source = MockTaskSource::new(tasks, HashMap::new());
        let plan = sync_to_local(&source, "1", tmp.path()).unwrap();

        assert_eq!(
            file_names(&plan.files),
            BTreeSet::from([
                "1.md".to_string(),
                "2.md".to_string(),
                "3.md".to_string(),
                "4.md".to_string(),
                "5.md".to_string()
            ])
        );
        assert!(!plan.path.join("6.md").exists());
    }

    #[test]
    fn sync_deduplicates_circular_references() {
        let tmp = TempDir::new().unwrap();

        let mut tasks = HashMap::new();
        tasks.insert(
            "1".to_string(),
            task("1", "One", "https://github.com/org/repo/issues/2"),
        );
        tasks.insert(
            "2".to_string(),
            task("2", "Two", "https://github.com/org/repo/issues/1"),
        );

        let source = MockTaskSource::new(tasks, HashMap::new());
        let plan = sync_to_local(&source, "1", tmp.path()).unwrap();

        assert_eq!(
            file_names(&plan.files),
            BTreeSet::from(["1.md".to_string(), "2.md".to_string()])
        );

        let calls = source.detail_calls();
        let count_task_2 = calls.iter().filter(|id| id.as_str() == "2").count();
        assert_eq!(count_task_2, 1);
    }

    #[test]
    fn sync_slug_falls_back_to_issue_id_when_title_slug_empty() {
        let tmp = TempDir::new().unwrap();

        let mut tasks = HashMap::new();
        tasks.insert("42".to_string(), task("42", "!!!", ""));

        let source = MockTaskSource::new(tasks, HashMap::new());
        let plan = sync_to_local(&source, "42", tmp.path()).unwrap();

        assert!(plan.path.ends_with("gh-42"));
    }
}
