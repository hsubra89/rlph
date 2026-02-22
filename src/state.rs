use std::collections::HashMap;
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct StateData {
    pub current_task: Option<CurrentTask>,
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

    /// Save state to disk.
    pub fn save(&self, state: &StateData) -> Result<()> {
        std::fs::create_dir_all(&self.state_dir)
            .map_err(|e| Error::State(format!("failed to create state dir: {e}")))?;

        let content = toml::to_string_pretty(state)
            .map_err(|e| Error::State(format!("failed to serialize state: {e}")))?;

        std::fs::write(self.state_file(), content)
            .map_err(|e| Error::State(format!("failed to write state file: {e}")))?;

        Ok(())
    }

    /// Set the current task and record its worktree mapping.
    pub fn set_current_task(&self, id: &str, phase: &str, worktree_path: &str) -> Result<()> {
        let mut state = self.load();
        state.current_task = Some(CurrentTask {
            id: id.to_string(),
            phase: phase.to_string(),
            worktree_path: worktree_path.to_string(),
        });
        state
            .worktree_mappings
            .insert(id.to_string(), worktree_path.to_string());
        self.save(&state)
    }

    /// Update only the phase of the current task.
    pub fn update_phase(&self, phase: &str) -> Result<()> {
        let mut state = self.load();
        if let Some(ref mut task) = state.current_task {
            task.phase = phase.to_string();
        }
        self.save(&state)
    }

    /// Mark the current task as completed and move it to history.
    pub fn complete_current_task(&self) -> Result<()> {
        let mut state = self.load();
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
        self.save(&state)
    }

    /// Clear the current task without adding to history.
    pub fn clear_current_task(&self) -> Result<()> {
        let mut state = self.load();
        state.current_task = None;
        self.save(&state)
    }

    /// Remove a worktree mapping.
    pub fn remove_worktree_mapping(&self, task_id: &str) -> Result<()> {
        let mut state = self.load();
        state.worktree_mappings.remove(task_id);
        self.save(&state)
    }

    /// Get the worktree path for a task.
    pub fn get_worktree_path(&self, task_id: &str) -> Option<String> {
        let state = self.load();
        state.worktree_mappings.get(task_id).cloned()
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
        assert!(state.worktree_mappings.get("gh-7").is_none());
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
}
