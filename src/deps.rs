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

#[derive(Default)]
struct TarjanState {
    index: usize,
    indices: HashMap<u64, usize>,
    lowlink: HashMap<u64, usize>,
    stack: Vec<u64>,
    on_stack: HashSet<u64>,
    components: Vec<Vec<u64>>,
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

    fn cycle_peers(&self) -> (HashMap<u64, HashSet<u64>>, Vec<Vec<u64>>) {
        let all_nodes: HashSet<u64> = self
            .edges
            .keys()
            .chain(self.edges.values().flat_map(|deps| deps.iter()))
            .copied()
            .collect();

        let mut nodes: Vec<u64> = all_nodes.into_iter().collect();
        nodes.sort_unstable();

        let mut state = TarjanState::default();

        for node in nodes {
            if !state.indices.contains_key(&node) {
                self.tarjan_strong_connect(node, &mut state);
            }
        }

        let mut cycle_peers: HashMap<u64, HashSet<u64>> = HashMap::new();
        let mut cycles_for_log = Vec::new();

        for component in state.components {
            let has_self_loop = component
                .iter()
                .any(|node| self.edges.get(node).is_some_and(|deps| deps.contains(node)));
            if component.len() <= 1 && !has_self_loop {
                continue;
            }

            let mut component_sorted = component.clone();
            component_sorted.sort_unstable();
            cycles_for_log.push(component_sorted);

            // Peer set includes the node itself; harmless because a node's deps
            // never contain itself (except self-loops, where this is correct).
            let component_set: HashSet<u64> = component.into_iter().collect();
            for &node in &component_set {
                cycle_peers
                    .entry(node)
                    .or_default()
                    .extend(component_set.iter().copied());
            }
        }

        cycles_for_log.sort_unstable();

        (cycle_peers, cycles_for_log)
    }

    fn tarjan_strong_connect(&self, node: u64, state: &mut TarjanState) {
        state.indices.insert(node, state.index);
        state.lowlink.insert(node, state.index);
        state.index += 1;
        state.stack.push(node);
        state.on_stack.insert(node);

        if let Some(deps) = self.edges.get(&node) {
            let mut sorted_deps: Vec<u64> = deps.iter().copied().collect();
            sorted_deps.sort_unstable();

            for dep in sorted_deps {
                if !state.indices.contains_key(&dep) {
                    self.tarjan_strong_connect(dep, state);
                    let dep_low = state.lowlink[&dep];
                    if let Some(node_low) = state.lowlink.get_mut(&node) {
                        *node_low = (*node_low).min(dep_low);
                    }
                } else if state.on_stack.contains(&dep) {
                    let dep_index = state.indices[&dep];
                    if let Some(node_low) = state.lowlink.get_mut(&node) {
                        *node_low = (*node_low).min(dep_index);
                    }
                }
            }
        }

        if state.lowlink[&node] == state.indices[&node] {
            let mut component = Vec::new();
            while let Some(stack_node) = state.stack.pop() {
                state.on_stack.remove(&stack_node);
                component.push(stack_node);
                if stack_node == node {
                    break;
                }
            }
            state.components.push(component);
        }
    }

    /// Filter tasks, returning only those whose dependencies are all in `done_ids`.
    /// Cycle-internal blockers are ignored (with a warning logged), but external blockers
    /// on cycle tasks are still enforced.
    pub fn filter_eligible(&self, tasks: Vec<Task>, done_ids: &HashSet<u64>) -> Vec<Task> {
        let (cycle_peers, cycles_for_log) = self.cycle_peers();

        if !cycles_for_log.is_empty() {
            warn!(
                cycles = ?cycles_for_log,
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
                        if let Some(peers) = cycle_peers.get(&id) {
                            // Ignore same-cycle blockers but still enforce external ones
                            deps.iter()
                                .filter(|dep| !peers.contains(dep))
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
    fn test_three_node_cycle_all_eligible() {
        let tasks = vec![
            make_task(1, "Blocked by #3"),
            make_task(2, "Blocked by #1"),
            make_task(3, "Blocked by #2"),
        ];
        let graph = DependencyGraph::build(&tasks);
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
    fn test_cross_cycle_dependency_still_enforced() {
        // Two separate cycles: {1,2} and {3,4}
        // Task 1 also depends on task 3 (cross-cycle dep) — must be enforced
        let tasks = vec![
            make_task(1, "Blocked by #2\nBlocked by #3"),
            make_task(2, "Blocked by #1"),
            make_task(3, "Blocked by #4"),
            make_task(4, "Blocked by #3"),
        ];
        let graph = DependencyGraph::build(&tasks);
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks.clone(), &done);
        // Task 1: cycle peer is 2, dep on 3 is cross-cycle → blocked
        // Task 2: only cycle-internal dep on 1 → eligible
        // Task 3: cycle peer is 4, no external deps → eligible
        // Task 4: cycle peer is 3, no external deps → eligible
        assert_eq!(eligible.len(), 3);
        let ids: Vec<&str> = eligible.iter().map(|t| t.id.as_str()).collect();
        assert!(!ids.contains(&"1"));
        assert!(ids.contains(&"2"));
        assert!(ids.contains(&"3"));
        assert!(ids.contains(&"4"));

        // Mark task 3 as done: task 1's cross-cycle dep resolved
        let done: HashSet<u64> = [3].into_iter().collect();
        let eligible = graph.filter_eligible(tasks, &done);
        assert_eq!(eligible.len(), 4);
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

    #[test]
    fn test_scc_cycle_internal_dep_not_misclassified_as_external() {
        // Single SCC: 1 -> {2,3}, 2 -> {1}, 3 -> {2}
        // All dependencies are cycle-internal, so all tasks should remain eligible.
        let tasks = vec![
            make_task(1, "Blocked by #2\nBlocked by #3"),
            make_task(2, "Blocked by #1"),
            make_task(3, "Blocked by #2"),
        ];
        let graph = DependencyGraph::build(&tasks);
        let done = HashSet::new();
        let eligible = graph.filter_eligible(tasks, &done);

        assert_eq!(eligible.len(), 3);
        let ids: Vec<&str> = eligible.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"1"));
        assert!(ids.contains(&"2"));
        assert!(ids.contains(&"3"));
    }
}
