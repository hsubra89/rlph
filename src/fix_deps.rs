use std::collections::{HashMap, HashSet};

use tracing::warn;

use crate::fix_comment::{CheckboxState, FixItem};

/// Dependency graph for review findings within a single PR.
///
/// Tracks `depends_on` relationships between findings and detects cycles.
/// Findings in cycles are warned about and should be skipped.
pub struct FindingDeps {
    /// finding_id → set of finding_ids it depends on (only known IDs)
    edges: HashMap<String, HashSet<String>>,
    /// Finding IDs that are part of a dependency cycle.
    cycle_members: HashSet<String>,
}

impl FindingDeps {
    /// Build from a list of fix items.
    ///
    /// Dependencies referencing unknown finding IDs (not present in `items`)
    /// are silently ignored. Cycles are detected and logged as warnings.
    pub fn build(items: &[FixItem]) -> Self {
        let known_ids: HashSet<&str> = items.iter().map(|i| i.finding.id.as_str()).collect();

        let mut edges: HashMap<String, HashSet<String>> = HashMap::new();
        for item in items {
            let deps: HashSet<String> = item
                .finding
                .depends_on
                .iter()
                .filter(|dep| known_ids.contains(dep.as_str()))
                .cloned()
                .collect();
            if !deps.is_empty() {
                edges.insert(item.finding.id.clone(), deps);
            }
        }

        let cycle_members = detect_cycles(&edges);
        if !cycle_members.is_empty() {
            let mut sorted: Vec<&str> = cycle_members.iter().map(|s| s.as_str()).collect();
            sorted.sort();
            warn!(
                items = ?sorted,
                "circular dependencies detected among findings; these items will be skipped"
            );
        }

        Self {
            edges,
            cycle_members,
        }
    }

    /// Returns `true` if this finding is part of a dependency cycle.
    pub fn in_cycle(&self, finding_id: &str) -> bool {
        self.cycle_members.contains(finding_id)
    }

    /// Returns `true` if all dependencies of this finding are resolved.
    ///
    /// A dependency is resolved if its ID is in the `resolved` set.
    /// Findings with no dependencies always return `true`.
    pub fn deps_met(&self, finding_id: &str, resolved: &HashSet<&str>) -> bool {
        let Some(deps) = self.edges.get(finding_id) else {
            return true;
        };
        deps.iter().all(|dep| resolved.contains(dep.as_str()))
    }

    /// Returns `true` if any finding has dependencies.
    #[cfg(test)]
    fn has_any_deps(&self) -> bool {
        !self.edges.is_empty()
    }
}

/// Collect finding IDs whose state is Fixed or WontFix (i.e. resolved).
pub fn resolved_finding_ids(items: &[FixItem]) -> HashSet<&str> {
    items
        .iter()
        .filter(|i| matches!(i.state, CheckboxState::Fixed | CheckboxState::WontFix))
        .map(|i| i.finding.id.as_str())
        .collect()
}

// --- Tarjan's SCC for cycle detection ---

#[derive(Default)]
struct TarjanState {
    index: usize,
    indices: HashMap<String, usize>,
    lowlink: HashMap<String, usize>,
    stack: Vec<String>,
    on_stack: HashSet<String>,
    components: Vec<Vec<String>>,
}

/// Detect all finding IDs that are part of a dependency cycle.
fn detect_cycles(edges: &HashMap<String, HashSet<String>>) -> HashSet<String> {
    let mut all_nodes: HashSet<&str> = HashSet::new();
    for (finding_id, deps) in edges {
        all_nodes.insert(finding_id.as_str());
        for dep in deps {
            all_nodes.insert(dep.as_str());
        }
    }

    let mut nodes: Vec<&str> = all_nodes.into_iter().collect();
    nodes.sort();

    let mut state = TarjanState::default();
    for &node in &nodes {
        if !state.indices.contains_key(node) {
            tarjan_strong_connect(node, edges, &mut state);
        }
    }

    let mut cycle_members = HashSet::new();
    for component in &state.components {
        let has_self_loop = component.iter().any(|node| {
            edges
                .get(node.as_str())
                .is_some_and(|deps| deps.contains(node))
        });
        if component.len() > 1 || has_self_loop {
            cycle_members.extend(component.iter().cloned());
        }
    }
    cycle_members
}

fn tarjan_strong_connect(
    node: &str,
    edges: &HashMap<String, HashSet<String>>,
    state: &mut TarjanState,
) {
    let node_owned = node.to_string();
    state.indices.insert(node_owned.clone(), state.index);
    state.lowlink.insert(node_owned.clone(), state.index);
    state.index += 1;
    state.stack.push(node_owned.clone());
    state.on_stack.insert(node_owned);

    if let Some(deps) = edges.get(node) {
        let mut sorted_deps: Vec<&str> = deps.iter().map(|s| s.as_str()).collect();
        sorted_deps.sort();

        for dep in sorted_deps {
            if !state.indices.contains_key(dep) {
                tarjan_strong_connect(dep, edges, state);
                let dep_low = state.lowlink[dep];
                if let Some(node_low) = state.lowlink.get_mut(node) {
                    *node_low = (*node_low).min(dep_low);
                }
            } else if state.on_stack.contains(dep) {
                let dep_index = state.indices[dep];
                if let Some(node_low) = state.lowlink.get_mut(node) {
                    *node_low = (*node_low).min(dep_index);
                }
            }
        }
    }

    if state.lowlink[node] == state.indices[node] {
        let mut component = Vec::new();
        while let Some(stack_node) = state.stack.pop() {
            state.on_stack.remove(&stack_node);
            let is_root = stack_node == node;
            component.push(stack_node);
            if is_root {
                break;
            }
        }
        state.components.push(component);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix_comment::FixItem;
    use crate::test_helpers::make_finding_with_deps;

    fn make_item(id: &str, deps: &[&str], state: CheckboxState) -> FixItem {
        FixItem {
            finding: make_finding_with_deps(id, deps),
            state,
        }
    }

    fn checked(id: &str, deps: &[&str]) -> FixItem {
        make_item(id, deps, CheckboxState::Checked)
    }

    fn fixed(id: &str, deps: &[&str]) -> FixItem {
        make_item(id, deps, CheckboxState::Fixed)
    }

    fn wontfix(id: &str, deps: &[&str]) -> FixItem {
        make_item(id, deps, CheckboxState::WontFix)
    }

    fn unchecked(id: &str, deps: &[&str]) -> FixItem {
        make_item(id, deps, CheckboxState::Unchecked)
    }

    // --- FindingDeps::build ---

    #[test]
    fn test_build_no_deps() {
        let items = vec![checked("a", &[]), checked("b", &[])];
        let deps = FindingDeps::build(&items);
        assert!(!deps.has_any_deps());
        assert!(deps.cycle_members.is_empty());
    }

    #[test]
    fn test_build_with_deps() {
        let items = vec![checked("a", &[]), checked("b", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(deps.has_any_deps());
        assert!(deps.edges.contains_key("b"));
        assert!(deps.edges["b"].contains("a"));
    }

    #[test]
    fn test_build_ignores_unknown_deps() {
        let items = vec![checked("a", &["nonexistent"])];
        let deps = FindingDeps::build(&items);
        assert!(!deps.has_any_deps());
    }

    #[test]
    fn test_build_partial_unknown_deps() {
        let items = vec![checked("a", &[]), checked("b", &["a", "nonexistent"])];
        let deps = FindingDeps::build(&items);
        assert!(deps.has_any_deps());
        assert_eq!(deps.edges["b"].len(), 1);
        assert!(deps.edges["b"].contains("a"));
    }

    // --- Cycle detection ---

    #[test]
    fn test_no_cycle() {
        let items = vec![checked("a", &[]), checked("b", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(!deps.in_cycle("a"));
        assert!(!deps.in_cycle("b"));
    }

    #[test]
    fn test_simple_cycle() {
        let items = vec![checked("a", &["b"]), checked("b", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(deps.in_cycle("a"));
        assert!(deps.in_cycle("b"));
    }

    #[test]
    fn test_self_loop() {
        let items = vec![checked("a", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(deps.in_cycle("a"));
    }

    #[test]
    fn test_three_node_cycle() {
        let items = vec![
            checked("a", &["c"]),
            checked("b", &["a"]),
            checked("c", &["b"]),
        ];
        let deps = FindingDeps::build(&items);
        assert!(deps.in_cycle("a"));
        assert!(deps.in_cycle("b"));
        assert!(deps.in_cycle("c"));
    }

    #[test]
    fn test_cycle_does_not_affect_non_cycle_items() {
        let items = vec![
            checked("a", &["b"]),
            checked("b", &["a"]),
            checked("c", &[]),
            checked("d", &["c"]),
        ];
        let deps = FindingDeps::build(&items);
        assert!(deps.in_cycle("a"));
        assert!(deps.in_cycle("b"));
        assert!(!deps.in_cycle("c"));
        assert!(!deps.in_cycle("d"));
    }

    // --- deps_met ---

    #[test]
    fn test_deps_met_no_deps() {
        let items = vec![checked("a", &[])];
        let deps = FindingDeps::build(&items);
        let resolved = HashSet::new();
        assert!(deps.deps_met("a", &resolved));
    }

    #[test]
    fn test_deps_met_with_fixed_dep() {
        let items = vec![fixed("a", &[]), checked("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_deps_met_with_wontfix_dep() {
        let items = vec![wontfix("a", &[]), checked("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_deps_not_met_unchecked_dep() {
        let items = vec![unchecked("a", &[]), checked("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(!deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_deps_not_met_checked_dep() {
        let items = vec![checked("a", &[]), checked("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(!deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_deps_partially_met() {
        let items = vec![
            fixed("a", &[]),
            checked("b", &[]),
            checked("c", &["a", "b"]),
        ];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        // c depends on a (Fixed) and b (Checked) → not met
        assert!(!deps.deps_met("c", &resolved));
    }

    #[test]
    fn test_deps_all_met() {
        let items = vec![
            fixed("a", &[]),
            wontfix("b", &[]),
            checked("c", &["a", "b"]),
        ];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        // c depends on a (Fixed) and b (WontFix) → met
        assert!(deps.deps_met("c", &resolved));
    }

    #[test]
    fn test_deps_met_unknown_finding_id() {
        let items = vec![checked("a", &[])];
        let deps = FindingDeps::build(&items);
        let resolved = HashSet::new();
        // Unknown finding has no edges → treated as met
        assert!(deps.deps_met("nonexistent", &resolved));
    }

    // --- resolved_finding_ids ---

    #[test]
    fn test_resolved_ids_empty() {
        let items = vec![checked("a", &[]), unchecked("b", &[])];
        let resolved = resolved_finding_ids(&items);
        assert!(resolved.is_empty());
    }

    #[test]
    fn test_resolved_ids_fixed_and_wontfix() {
        let items = vec![
            fixed("a", &[]),
            wontfix("b", &[]),
            checked("c", &[]),
            unchecked("d", &[]),
        ];
        let resolved = resolved_finding_ids(&items);
        assert_eq!(resolved.len(), 2);
        assert!(resolved.contains("a"));
        assert!(resolved.contains("b"));
    }

    // --- Integration scenarios ---

    #[test]
    fn test_chain_dependency_a_then_b_then_c() {
        // a → b → c: only a is eligible initially
        let items = vec![
            checked("a", &[]),
            checked("b", &["a"]),
            checked("c", &["b"]),
        ];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        assert!(deps.deps_met("a", &resolved));
        assert!(!deps.deps_met("b", &resolved));
        assert!(!deps.deps_met("c", &resolved));

        // After a is fixed
        let items2 = vec![
            fixed("a", &[]),
            checked("b", &["a"]),
            checked("c", &["b"]),
        ];
        let resolved2 = resolved_finding_ids(&items2);
        assert!(deps.deps_met("b", &resolved2));
        assert!(!deps.deps_met("c", &resolved2));

        // After b is also fixed
        let items3 = vec![
            fixed("a", &[]),
            fixed("b", &["a"]),
            checked("c", &["b"]),
        ];
        let resolved3 = resolved_finding_ids(&items3);
        assert!(deps.deps_met("c", &resolved3));
    }

    #[test]
    fn test_diamond_dependency() {
        // d depends on b and c, both depend on a
        let items = vec![
            checked("a", &[]),
            checked("b", &["a"]),
            checked("c", &["a"]),
            checked("d", &["b", "c"]),
        ];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        assert!(deps.deps_met("a", &resolved));
        assert!(!deps.deps_met("b", &resolved));
        assert!(!deps.deps_met("c", &resolved));
        assert!(!deps.deps_met("d", &resolved));

        // After a is fixed, b and c are eligible
        let items2 = vec![
            fixed("a", &[]),
            checked("b", &["a"]),
            checked("c", &["a"]),
            checked("d", &["b", "c"]),
        ];
        let resolved2 = resolved_finding_ids(&items2);
        assert!(deps.deps_met("b", &resolved2));
        assert!(deps.deps_met("c", &resolved2));
        assert!(!deps.deps_met("d", &resolved2));

        // After b and c are fixed, d is eligible
        let items3 = vec![
            fixed("a", &[]),
            fixed("b", &["a"]),
            fixed("c", &["a"]),
            checked("d", &["b", "c"]),
        ];
        let resolved3 = resolved_finding_ids(&items3);
        assert!(deps.deps_met("d", &resolved3));
    }

    #[test]
    fn test_wontfix_unblocks_dependent() {
        let items = vec![wontfix("a", &[]), checked("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_cycle_members_skipped_non_cycle_proceed() {
        let items = vec![
            checked("a", &["b"]),
            checked("b", &["a"]),
            checked("c", &[]),
        ];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        assert!(deps.in_cycle("a"));
        assert!(deps.in_cycle("b"));
        assert!(!deps.in_cycle("c"));
        assert!(deps.deps_met("c", &resolved));
    }
}
