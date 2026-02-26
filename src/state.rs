use std::collections::HashMap;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurrentTask {
    pub id: String,
    pub phase: String,
    pub worktree_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompletedTask {
    pub id: String,
    pub completed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurrentGroupState {
    pub group_id: String,
    pub group_sub_issues: Vec<String>,
    pub completed_sub_issues: Vec<String>,
    pub group_worktree_path: String,
    pub group_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct StateData {
    pub current_task: Option<CurrentTask>,
    #[serde(default)]
    pub current_group: Option<CurrentGroupState>,
    #[serde(default)]
    pub history: Vec<CompletedTask>,
    #[serde(default)]
    pub worktree_mappings: HashMap<String, String>,
}

/// Manages local state persisted as TOML in `.rlph/state/`.
pub struct StateManager {
    state_dir: PathBuf,
}

impl StateManager {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
        }
    }

    /// Default state directory relative to a repo root.
    pub fn default_dir(repo_root: &Path) -> PathBuf {
        repo_root.join(".rlph").join("state")
    }

    fn state_file(&self) -> PathBuf {
        self.state_dir.join("state.toml")
    }

    /// Load state from disk. Returns default state if file is missing or corrupted.
    pub fn load(&self) -> StateData {
        let path = self.state_file();
        if !path.exists() {
            return StateData::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<StateData>(&content) {
                Ok(state) => state,
                Err(e) => {
                    warn!("corrupted state file {}: {e}, resetting", path.display());
                    StateData::default()
                }
            },
            Err(e) => {
                warn!(
                    "failed to read state file {}: {e}, resetting",
                    path.display()
                );
                StateData::default()
            }
        }
    }

    fn lock_file_path(&self) -> PathBuf {
        self.state_dir.join(".state.lock")
    }

    /// Atomically load, modify, and save state under an exclusive file lock.
    /// Prevents concurrent load-modify-save races between processes.
    fn modify(&self, f: impl FnOnce(&mut StateData)) -> Result<()> {
        std::fs::create_dir_all(&self.state_dir)
            .map_err(|e| Error::State(format!("failed to create state dir: {e}")))?;

        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(self.lock_file_path())
            .map_err(|e| Error::State(format!("failed to open lock file: {e}")))?;

        let ret = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(Error::State(format!(
                "failed to acquire state lock: {}",
                std::io::Error::last_os_error()
            )));
        }

        let mut state = self.load();
        f(&mut state);
        self.save(&state)
        // Lock released when `lock` is dropped (fd closed)
    }

    /// Save state to disk atomically (write tmp + fsync + rename).
    pub fn save(&self, state: &StateData) -> Result<()> {
        use std::io::Write;

        std::fs::create_dir_all(&self.state_dir)
            .map_err(|e| Error::State(format!("failed to create state dir: {e}")))?;

        let content = toml::to_string_pretty(state)
            .map_err(|e| Error::State(format!("failed to serialize state: {e}")))?;

        let dest = self.state_file();
        let tmp = self.state_dir.join(".state.toml.tmp");

        let mut file = std::fs::File::create(&tmp)
            .map_err(|e| Error::State(format!("failed to create temp state file: {e}")))?;
        file.write_all(content.as_bytes())
            .map_err(|e| Error::State(format!("failed to write temp state file: {e}")))?;
        file.sync_all()
            .map_err(|e| Error::State(format!("failed to fsync temp state file: {e}")))?;

        std::fs::rename(&tmp, &dest)
            .map_err(|e| Error::State(format!("failed to rename temp state file: {e}")))?;

        Ok(())
    }

    /// Set the current task and record its worktree mapping.
    pub fn set_current_task(&self, id: &str, phase: &str, worktree_path: &str) -> Result<()> {
        let id = id.to_string();
        let phase = phase.to_string();
        let worktree_path = worktree_path.to_string();
        self.modify(|state| {
            state.current_task = Some(CurrentTask {
                id: id.clone(),
                phase,
                worktree_path: worktree_path.clone(),
            });
            state.worktree_mappings.insert(id, worktree_path);
        })
    }

    /// Update only the phase of the current task.
    pub fn update_phase(&self, phase: &str) -> Result<()> {
        let phase = phase.to_string();
        self.modify(|state| {
            if let Some(ref mut task) = state.current_task {
                task.phase = phase;
            }
        })
    }

    /// Mark the current task as completed and move it to history.
    pub fn complete_current_task(&self) -> Result<()> {
        self.modify(|state| {
            if let Some(task) = state.current_task.take() {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                state.history.push(CompletedTask {
                    id: task.id,
                    completed_at: timestamp,
                });
            }
        })
    }

    /// Clear the current task without adding to history.
    pub fn clear_current_task(&self) -> Result<()> {
        self.modify(|state| {
            state.current_task = None;
        })
    }

    /// Remove a worktree mapping.
    pub fn remove_worktree_mapping(&self, task_id: &str) -> Result<()> {
        let task_id = task_id.to_string();
        self.modify(|state| {
            state.worktree_mappings.remove(&task_id);
        })
    }

    /// Get the worktree path for a task.
    pub fn get_worktree_path(&self, task_id: &str) -> Option<String> {
        let state = self.load();
        state.worktree_mappings.get(task_id).cloned()
    }

    /// Set the current group being worked on.
    pub fn set_current_group(
        &self,
        group_id: &str,
        sub_issues: &[String],
        worktree_path: &str,
        branch: &str,
    ) -> Result<()> {
        let group_id = group_id.to_string();
        let sub_issues = sub_issues.to_vec();
        let worktree_path = worktree_path.to_string();
        let branch = branch.to_string();
        self.modify(|state| {
            state.current_group = Some(CurrentGroupState {
                group_id,
                group_sub_issues: sub_issues,
                completed_sub_issues: Vec::new(),
                group_worktree_path: worktree_path,
                group_branch: branch,
            });
        })
    }

    /// Mark a sub-issue as complete within the current group.
    pub fn mark_sub_issue_complete(&self, sub_id: &str) -> Result<()> {
        let sub_id = sub_id.to_string();
        self.modify(|state| {
            if let Some(ref mut group) = state.current_group
                && !group.completed_sub_issues.contains(&sub_id)
            {
                group.completed_sub_issues.push(sub_id);
            }
        })
    }

    /// Clear the current group (group is done or abandoned).
    pub fn complete_current_group(&self) -> Result<()> {
        self.modify(|state| {
            state.current_group = None;
        })
    }

    /// Get the current group state, if any.
    pub fn get_current_group(&self) -> Option<CurrentGroupState> {
        self.load().current_group
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_manager() -> (TempDir, StateManager) {
        let dir = TempDir::new().unwrap();
        let mgr = StateManager::new(dir.path().join("state"));
        (dir, mgr)
    }

    #[test]
    fn test_load_empty_returns_default() {
        let (_dir, mgr) = test_manager();
        let state = mgr.load();
        assert_eq!(state, StateData::default());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let (_dir, mgr) = test_manager();
        let state = StateData {
            current_task: Some(CurrentTask {
                id: "gh-5".to_string(),
                phase: "implement".to_string(),
                worktree_path: "/tmp/wt".to_string(),
            }),
            current_group: None,
            history: vec![CompletedTask {
                id: "gh-3".to_string(),
                completed_at: 1700000000,
            }],
            worktree_mappings: HashMap::from([
                ("gh-5".to_string(), "/tmp/wt".to_string()),
                ("gh-3".to_string(), "/tmp/old".to_string()),
            ]),
        };
        mgr.save(&state).unwrap();
        let loaded = mgr.load();
        assert_eq!(loaded, state);
    }

    #[test]
    fn test_corrupted_state_returns_default() {
        let (_dir, mgr) = test_manager();
        std::fs::create_dir_all(mgr.state_dir.clone()).unwrap();
        std::fs::write(mgr.state_file(), "this is not valid toml [[[").unwrap();

        let state = mgr.load();
        assert_eq!(state, StateData::default());
    }

    #[test]
    fn test_set_current_task() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_task("gh-7", "choose", "/tmp/wt7").unwrap();

        let state = mgr.load();
        let task = state.current_task.unwrap();
        assert_eq!(task.id, "gh-7");
        assert_eq!(task.phase, "choose");
        assert_eq!(task.worktree_path, "/tmp/wt7");
        assert_eq!(state.worktree_mappings.get("gh-7").unwrap(), "/tmp/wt7");
    }

    #[test]
    fn test_update_phase() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_task("gh-7", "choose", "/tmp/wt7").unwrap();
        mgr.update_phase("implement").unwrap();

        let state = mgr.load();
        assert_eq!(state.current_task.unwrap().phase, "implement");
    }

    #[test]
    fn test_complete_current_task() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_task("gh-7", "implement", "/tmp/wt7")
            .unwrap();
        mgr.complete_current_task().unwrap();

        let state = mgr.load();
        assert!(state.current_task.is_none());
        assert_eq!(state.history.len(), 1);
        assert_eq!(state.history[0].id, "gh-7");
        assert!(state.history[0].completed_at > 0);
    }

    #[test]
    fn test_complete_no_current_task() {
        let (_dir, mgr) = test_manager();
        // Should not panic when no current task
        mgr.complete_current_task().unwrap();
        let state = mgr.load();
        assert!(state.history.is_empty());
    }

    #[test]
    fn test_clear_current_task() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_task("gh-7", "review", "/tmp/wt7").unwrap();
        mgr.clear_current_task().unwrap();

        let state = mgr.load();
        assert!(state.current_task.is_none());
        // History should NOT have the task
        assert!(state.history.is_empty());
    }

    #[test]
    fn test_remove_worktree_mapping() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_task("gh-7", "implement", "/tmp/wt7")
            .unwrap();
        mgr.remove_worktree_mapping("gh-7").unwrap();

        let state = mgr.load();
        assert!(!state.worktree_mappings.contains_key("gh-7"));
    }

    #[test]
    fn test_get_worktree_path() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_task("gh-7", "implement", "/tmp/wt7")
            .unwrap();
        assert_eq!(mgr.get_worktree_path("gh-7").unwrap(), "/tmp/wt7");
        assert!(mgr.get_worktree_path("gh-999").is_none());
    }

    #[test]
    fn test_state_survives_reload() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("state");

        // First "process"
        {
            let mgr = StateManager::new(&state_path);
            mgr.set_current_task("gh-10", "implement", "/tmp/wt10")
                .unwrap();
        }

        // Second "process" (simulates restart)
        {
            let mgr = StateManager::new(&state_path);
            let state = mgr.load();
            let task = state.current_task.unwrap();
            assert_eq!(task.id, "gh-10");
            assert_eq!(task.phase, "implement");
        }
    }

    #[test]
    fn test_multiple_completed_tasks() {
        let (_dir, mgr) = test_manager();

        mgr.set_current_task("gh-1", "implement", "/tmp/wt1")
            .unwrap();
        mgr.complete_current_task().unwrap();

        mgr.set_current_task("gh-2", "implement", "/tmp/wt2")
            .unwrap();
        mgr.complete_current_task().unwrap();

        let state = mgr.load();
        assert_eq!(state.history.len(), 2);
        assert_eq!(state.history[0].id, "gh-1");
        assert_eq!(state.history[1].id, "gh-2");
    }

    #[test]
    fn test_state_file_is_valid_toml() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_task("gh-5", "implement", "/tmp/wt5")
            .unwrap();

        let content = std::fs::read_to_string(mgr.state_file()).unwrap();
        // Should be parseable TOML
        let _: toml::Value = toml::from_str(&content).unwrap();
    }

    // --- Group state tests ---

    #[test]
    fn test_group_state_roundtrip() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_group(
            "10",
            &["11".into(), "12".into(), "13".into()],
            "/tmp/wt-group",
            "group-branch",
        )
        .unwrap();

        let group = mgr.get_current_group().unwrap();
        assert_eq!(group.group_id, "10");
        assert_eq!(group.group_sub_issues, vec!["11", "12", "13"]);
        assert!(group.completed_sub_issues.is_empty());
        assert_eq!(group.group_worktree_path, "/tmp/wt-group");
        assert_eq!(group.group_branch, "group-branch");
    }

    #[test]
    fn test_group_mark_sub_issue_complete() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_group("10", &["11".into(), "12".into()], "/tmp/wt", "branch")
            .unwrap();

        mgr.mark_sub_issue_complete("11").unwrap();
        let group = mgr.get_current_group().unwrap();
        assert_eq!(group.completed_sub_issues, vec!["11"]);

        // Duplicate marking is idempotent
        mgr.mark_sub_issue_complete("11").unwrap();
        let group = mgr.get_current_group().unwrap();
        assert_eq!(group.completed_sub_issues, vec!["11"]);

        mgr.mark_sub_issue_complete("12").unwrap();
        let group = mgr.get_current_group().unwrap();
        assert_eq!(group.completed_sub_issues, vec!["11", "12"]);
    }

    #[test]
    fn test_group_complete_clears_group() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_group("10", &["11".into()], "/tmp/wt", "branch")
            .unwrap();
        mgr.complete_current_group().unwrap();
        assert!(mgr.get_current_group().is_none());
    }

    #[test]
    fn test_group_state_survives_reload() {
        let dir = TempDir::new().unwrap();
        let state_path = dir.path().join("state");

        {
            let mgr = StateManager::new(&state_path);
            mgr.set_current_group("10", &["11".into(), "12".into()], "/tmp/wt", "branch")
                .unwrap();
            mgr.mark_sub_issue_complete("11").unwrap();
        }

        {
            let mgr = StateManager::new(&state_path);
            let group = mgr.get_current_group().unwrap();
            assert_eq!(group.group_id, "10");
            assert_eq!(group.completed_sub_issues, vec!["11"]);
        }
    }

    #[test]
    fn test_group_state_save_load_with_task() {
        let (_dir, mgr) = test_manager();
        mgr.set_current_task("42", "implement", "/tmp/wt42")
            .unwrap();
        mgr.set_current_group("10", &["11".into()], "/tmp/wt-grp", "grp-branch")
            .unwrap();

        let state = mgr.load();
        assert!(state.current_task.is_some());
        assert!(state.current_group.is_some());
        assert_eq!(state.current_group.unwrap().group_id, "10");
    }

    #[test]
    fn test_concurrent_modifications_are_serialized() {
        use std::sync::Arc;
        use std::thread;

        let dir = TempDir::new().unwrap();
        let state_path: Arc<PathBuf> = Arc::new(dir.path().join("state"));

        // 20 threads each add a unique worktree mapping via set_current_task.
        // Without locking, concurrent load-modify-save would lose mappings.
        let handles: Vec<_> = (1..=20)
            .map(|i| {
                let sp: Arc<PathBuf> = Arc::clone(&state_path);
                thread::spawn(move || {
                    let mgr = StateManager::new(sp.as_ref());
                    let id = format!("gh-{i}");
                    let wt = format!("/tmp/wt{i}");
                    mgr.set_current_task(&id, "implement", &wt).unwrap();
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let mgr = StateManager::new(state_path.as_ref());
        let state = mgr.load();
        // All 20 worktree mappings must be present — none lost to races.
        assert_eq!(state.worktree_mappings.len(), 20);
    }
}
