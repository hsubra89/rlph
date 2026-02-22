use std::collections::{HashMap, HashSet};

use regex::Regex;
use tracing::warn;

use crate::sources::Task;

/// Parse dependency issue numbers from an issue body.
///
/// Recognized patterns (case-insensitive):
/// - `blocked by #N`
/// - `depends on #N`
/// - `blockedBy: [N, M, ...]`
pub fn parse_dependencies(body: &str) -> Vec<u64> {
    let mut deps = Vec::new();

    // "blocked by #N" or "depends on #N"
    let inline_re = Regex::new(r"(?i)(?:blocked\s+by|depends\s+on)\s+#(\d+)").unwrap();
    for cap in inline_re.captures_iter(body) {
        if let Ok(n) = cap[1].parse::<u64>() {
            deps.push(n);
        }
    }

    // "blockedBy: [N, M, ...]"
    let list_re = Regex::new(r"(?i)blockedBy:\s*\[([^\]]+)\]").unwrap();
    for cap in list_re.captures_iter(body) {
        for num_str in cap[1].split(',') {
            if let Ok(n) = num_str.trim().parse::<u64>() {
                deps.push(n);
            }
        }
    }

    deps.sort_unstable();
    deps.dedup();
    deps
}

/// A dependency graph mapping task IDs to their dependency IDs.
pub struct DependencyGraph {
    /// task_id -> set of task_ids it depends on
    edges: HashMap<u64, HashSet<u64>>,
}

impl DependencyGraph {
    /// Build a dependency graph from tasks by parsing each task's body for dependency patterns.
    pub fn build(tasks: &[Task]) -> Self {
        let mut edges = HashMap::new();
        for task in tasks {
            let id: u64 = match task.id.parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            let deps = parse_dependencies(&task.body);
            if !deps.is_empty() {
                edges.insert(id, deps.into_iter().collect());
            }
        }
        Self { edges }
    }

    /// Detect cycles in the dependency graph. Returns lists of node IDs forming cycles.
    pub fn detect_cycles(&self) -> Vec<Vec<u64>> {
        let all_nodes: HashSet<u64> = self
            .edges
            .keys()
            .chain(self.edges.values().flat_map(|deps| deps.iter()))
            .copied()
            .collect();

        let mut visited = HashSet::new();
        let mut on_stack = HashSet::new();
        let mut cycles = Vec::new();
        let mut path = Vec::new();

        // Sort for deterministic cycle detection order
        let mut nodes: Vec<u64> = all_nodes.into_iter().collect();
        nodes.sort_unstable();

        for node in nodes {
            if !visited.contains(&node) {
                self.dfs_cycle(node, &mut visited, &mut on_stack, &mut path, &mut cycles);
            }
        }

        cycles
    }

    fn dfs_cycle(
        &self,
        node: u64,
        visited: &mut HashSet<u64>,
        on_stack: &mut HashSet<u64>,
        path: &mut Vec<u64>,
        cycles: &mut Vec<Vec<u64>>,
    ) {
        visited.insert(node);
        on_stack.insert(node);
        path.push(node);

        if let Some(deps) = self.edges.get(&node) {
            let mut sorted_deps: Vec<u64> = deps.iter().copied().collect();
            sorted_deps.sort_unstable();
            for dep in sorted_deps {
                if !visited.contains(&dep) {
                    self.dfs_cycle(dep, visited, on_stack, path, cycles);
                } else if on_stack.contains(&dep)
                    && let Some(pos) = path.iter().position(|&n| n == dep)
                {
                    cycles.push(path[pos..].to_vec());
                }
            }
        }

        path.pop();
        on_stack.remove(&node);
    }

    /// Filter tasks, returning only those whose dependencies are all in `done_ids`.
    /// Cycle-internal blockers are ignored (with a warning logged), but external blockers
    /// on cycle tasks are still enforced.
    pub fn filter_eligible(&self, tasks: Vec<Task>, done_ids: &HashSet<u64>) -> Vec<Task> {
        let cycles = self.detect_cycles();
        let cycle_nodes: HashSet<u64> = cycles.iter().flat_map(|c| c.iter().copied()).collect();

        if !cycle_nodes.is_empty() {
            warn!(
                ?cycles,
                "dependency cycles detected; ignoring cycle-internal blockers (external blockers still enforced)"
            );
        }

        tasks
            .into_iter()
            .filter(|task| {
                let id: u64 = match task.id.parse() {
                    Ok(n) => n,
                    Err(_) => return true,
                };
                match self.edges.get(&id) {
                    None => true,
                    Some(deps) => {
                        if cycle_nodes.contains(&id) {
                            // Ignore cycle-internal blockers but still enforce external ones
                            deps.iter()
                                .filter(|dep| !cycle_nodes.contains(dep))
                                .all(|dep| done_ids.contains(dep))
                        } else {
                            deps.iter().all(|dep| done_ids.contains(dep))
                        }
                    }
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // --- parse_dependencies tests ---

    #[test]
    fn test_parse_blocked_by() {
        assert_eq!(parse_dependencies("Blocked by #5"), vec![5]);
        assert_eq!(parse_dependencies("blocked by #12"), vec![12]);
    }

    #[test]
    fn test_parse_depends_on() {
        assert_eq!(parse_dependencies("Depends on #3"), vec![3]);
        assert_eq!(parse_dependencies("depends on #7"), vec![7]);
    }

    #[test]
    fn test_parse_blocked_by_list() {
        assert_eq!(parse_dependencies("blockedBy: [1, 2, 3]"), vec![1, 2, 3]);
    }

    #[test]
    fn test_parse_case_insensitive() {
        assert_eq!(parse_dependencies("BLOCKED BY #99"), vec![99]);
        assert_eq!(parse_dependencies("DEPENDS ON #42"), vec![42]);
        assert_eq!(parse_dependencies("BLOCKEDBY: [10, 20]"), vec![10, 20]);
    }

    #[test]
    fn test_parse_multiple_patterns() {
        let body = "Blocked by #1\nDepends on #2\nblockedBy: [3, 4]";
        assert_eq!(parse_dependencies(body), vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_parse_no_dependencies() {
        assert!(parse_dependencies("No deps here").is_empty());
        assert!(parse_dependencies("").is_empty());
    }

    #[test]
    fn test_parse_deduplication() {
        let body = "Blocked by #5\nDepends on #5";
        assert_eq!(parse_dependencies(body), vec![5]);
    }

    // --- DependencyGraph tests ---

    #[test]
    fn test_graph_no_deps() {
        let tasks = vec![make_task(1, "No deps"), make_task(2, "Also none")];
        let graph = DependencyGraph::build(&tasks);
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks, &done);
        assert_eq!(eligible.len(), 2);
    }

    #[test]
    fn test_graph_filters_blocked() {
        let tasks = vec![
            make_task(1, "No deps"),
            make_task(2, "Blocked by #1"),
            make_task(3, "Blocked by #99"),
        ];
        let graph = DependencyGraph::build(&tasks);
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks, &done);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "1");
    }

    #[test]
    fn test_graph_unblocks_when_done() {
        let tasks = vec![make_task(1, "No deps"), make_task(2, "Blocked by #1")];
        let graph = DependencyGraph::build(&tasks);
        let done: HashSet<u64> = [1].into_iter().collect();
        let eligible = graph.filter_eligible(tasks, &done);
        assert_eq!(eligible.len(), 2);
    }

    #[test]
    fn test_graph_partial_unblock() {
        let tasks = vec![make_task(1, "No deps"), make_task(2, "blockedBy: [1, 99]")];
        let graph = DependencyGraph::build(&tasks);
        let done: HashSet<u64> = [1].into_iter().collect();
        let eligible = graph.filter_eligible(tasks, &done);
        // Task 2 still blocked by #99
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "1");
    }

    #[test]
    fn test_cycle_detection() {
        let tasks = vec![make_task(1, "Blocked by #2"), make_task(2, "Blocked by #1")];
        let graph = DependencyGraph::build(&tasks);
        let cycles = graph.detect_cycles();
        assert!(!cycles.is_empty());
    }

    #[test]
    fn test_cycle_treated_as_unblocked() {
        let tasks = vec![
            make_task(1, "Blocked by #2"),
            make_task(2, "Blocked by #1"),
            make_task(3, "No deps"),
        ];
        let graph = DependencyGraph::build(&tasks);
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks, &done);
        assert_eq!(eligible.len(), 3);
    }

    #[test]
    fn test_three_node_cycle() {
        let tasks = vec![
            make_task(1, "Blocked by #3"),
            make_task(2, "Blocked by #1"),
            make_task(3, "Blocked by #2"),
        ];
        let graph = DependencyGraph::build(&tasks);
        let cycles = graph.detect_cycles();
        assert!(!cycles.is_empty());
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks, &done);
        assert_eq!(eligible.len(), 3);
    }

    #[test]
    fn test_mixed_blocked_and_cycle() {
        let tasks = vec![
            make_task(1, "Blocked by #2"),
            make_task(2, "Blocked by #1"),
            make_task(3, "Blocked by #99"), // blocked by external, not a cycle
            make_task(4, "No deps"),
        ];
        let graph = DependencyGraph::build(&tasks);
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks, &done);
        // 1,2 in cycle (unblocked), 3 blocked by #99, 4 no deps
        assert_eq!(eligible.len(), 3);
        let ids: Vec<&str> = eligible.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"1"));
        assert!(ids.contains(&"2"));
        assert!(ids.contains(&"4"));
    }

    // --- Regression tests for cycle + external blocker (issue #21) ---

    #[test]
    fn test_cycle_task_with_external_blocker_is_blocked() {
        // Task 1 and 2 form a cycle, but task 1 also depends on external #99
        let tasks = vec![
            make_task(1, "Blocked by #2\nBlocked by #99"),
            make_task(2, "Blocked by #1"),
        ];
        let graph = DependencyGraph::build(&tasks);
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks, &done);
        // Task 1: in cycle but blocked by external #99 → blocked
        // Task 2: in cycle, only cycle-internal dep → eligible
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "2");
    }

    #[test]
    fn test_cycle_task_external_blocker_resolved() {
        // Same as above but #99 is now done
        let tasks = vec![
            make_task(1, "Blocked by #2\nBlocked by #99"),
            make_task(2, "Blocked by #1"),
        ];
        let graph = DependencyGraph::build(&tasks);
        let done: HashSet<u64> = [99].into_iter().collect();
        let eligible = graph.filter_eligible(tasks, &done);
        // Both eligible: cycle deps ignored, external #99 is done
        assert_eq!(eligible.len(), 2);
    }

    #[test]
    fn test_cycle_with_multiple_external_blockers() {
        // 3-node cycle where one node has external deps
        let tasks = vec![
            make_task(1, "Blocked by #3\nBlocked by #50"),
            make_task(2, "Blocked by #1"),
            make_task(3, "Blocked by #2\nBlocked by #60"),
        ];
        let graph = DependencyGraph::build(&tasks);

        // Nothing done: 1 blocked by #50, 3 blocked by #60, 2 only has cycle dep
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks.clone(), &done);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "2");

        // #50 done: 1 unblocked, 3 still blocked by #60
        let done: HashSet<u64> = [50].into_iter().collect();
        let eligible = graph.filter_eligible(tasks.clone(), &done);
        assert_eq!(eligible.len(), 2);
        let ids: Vec<&str> = eligible.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"1"));
        assert!(ids.contains(&"2"));

        // Both #50 and #60 done: all unblocked
        let done: HashSet<u64> = [50, 60].into_iter().collect();
        let eligible = graph.filter_eligible(tasks, &done);
        assert_eq!(eligible.len(), 3);
    }

    #[test]
    fn test_pure_cycle_no_external_still_eligible() {
        // Pure cycle with no external deps — existing behavior preserved
        let tasks = vec![make_task(1, "Blocked by #2"), make_task(2, "Blocked by #1")];
        let graph = DependencyGraph::build(&tasks);
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks, &done);
        assert_eq!(eligible.len(), 2);
    }

    #[test]
    fn test_non_cycle_tasks_unaffected_by_fix() {
        // Non-cycle tasks keep standard blocked/unblocked behavior
        let tasks = vec![
            make_task(10, "Blocked by #20"),
            make_task(20, "No deps"),
            make_task(30, "Blocked by #10"),
        ];
        let graph = DependencyGraph::build(&tasks);

        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks.clone(), &done);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "20");

        let done: HashSet<u64> = [20].into_iter().collect();
        let eligible = graph.filter_eligible(tasks, &done);
        assert_eq!(eligible.len(), 2);
        let ids: Vec<&str> = eligible.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"10"));
        assert!(ids.contains(&"20"));
    }
}
