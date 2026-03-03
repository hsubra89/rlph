use std::collections::{HashMap, HashSet};

use tracing::warn;

use crate::fix_comment::{FindingState, FixItem};
use crate::scc::TarjanScc;

/// Dependency graph for review findings within a single PR.
///
/// Tracks `depends_on` relationships between findings and detects cycles.
/// Findings in cycles are warned about and should be skipped.
pub struct FindingDeps {
    /// finding_id → set of finding_ids it depends on (only known IDs)
    edges: HashMap<String, HashSet<String>>,
    /// Finding IDs that are part of a dependency cycle.
    cycle_members: HashSet<String>,
    /// Number of items when the graph was built (for staleness detection).
    item_count: usize,
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

        let cycle_members = TarjanScc::compute(&edges).cycle_members();
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
            item_count: items.len(),
        }
    }

    /// Returns the item count the graph was built from.
    pub fn item_count(&self) -> usize {
        self.item_count
    }

    /// Returns `true` if the item count has changed since the graph was built.
    pub fn is_stale(&self, current_count: usize) -> bool {
        self.item_count != current_count
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

    /// Returns the unresolved dependency IDs for a finding, sorted.
    ///
    /// Only returns deps that are in the graph but not yet in `resolved`.
    /// Findings with no dependencies return an empty vec.
    pub fn unresolved_deps(&self, finding_id: &str, resolved: &HashSet<&str>) -> Vec<&str> {
        let Some(deps) = self.edges.get(finding_id) else {
            return vec![];
        };
        let mut unresolved: Vec<&str> = deps
            .iter()
            .filter(|dep| !resolved.contains(dep.as_str()))
            .map(|s| s.as_str())
            .collect();
        unresolved.sort();
        unresolved
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
        .filter(|i| matches!(i.state, FindingState::Fixed | FindingState::WontFix))
        .map(|i| i.finding.id.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix_comment::FixItem;
    use crate::test_helpers::make_finding_with_deps;

    fn make_item(id: &str, deps: &[&str], state: FindingState) -> FixItem {
        FixItem {
            finding: make_finding_with_deps(id, deps),
            state,
            comment_id: 0,
            rocket_reaction_ids: vec![],
        }
    }

    fn queued(id: &str, deps: &[&str]) -> FixItem {
        make_item(id, deps, FindingState::Queued)
    }

    fn fixed(id: &str, deps: &[&str]) -> FixItem {
        make_item(id, deps, FindingState::Fixed)
    }

    fn wontfix(id: &str, deps: &[&str]) -> FixItem {
        make_item(id, deps, FindingState::WontFix)
    }

    fn pending(id: &str, deps: &[&str]) -> FixItem {
        make_item(id, deps, FindingState::Pending)
    }

    // --- FindingDeps::build ---

    #[test]
    fn test_build_no_deps() {
        let items = vec![queued("a", &[]), queued("b", &[])];
        let deps = FindingDeps::build(&items);
        assert!(!deps.has_any_deps());
        assert!(deps.cycle_members.is_empty());
    }

    #[test]
    fn test_build_with_deps() {
        let items = vec![queued("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(deps.has_any_deps());
        assert!(deps.edges.contains_key("b"));
        assert!(deps.edges["b"].contains("a"));
    }

    #[test]
    fn test_build_ignores_unknown_deps() {
        let items = vec![queued("a", &["nonexistent"])];
        let deps = FindingDeps::build(&items);
        assert!(!deps.has_any_deps());
    }

    #[test]
    fn test_build_partial_unknown_deps() {
        let items = vec![queued("a", &[]), queued("b", &["a", "nonexistent"])];
        let deps = FindingDeps::build(&items);
        assert!(deps.has_any_deps());
        assert_eq!(deps.edges["b"].len(), 1);
        assert!(deps.edges["b"].contains("a"));
    }

    // --- Cycle detection ---

    #[test]
    fn test_no_cycle() {
        let items = vec![queued("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(!deps.in_cycle("a"));
        assert!(!deps.in_cycle("b"));
    }

    #[test]
    fn test_simple_cycle() {
        let items = vec![queued("a", &["b"]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(deps.in_cycle("a"));
        assert!(deps.in_cycle("b"));
    }

    #[test]
    fn test_self_loop() {
        let items = vec![queued("a", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(deps.in_cycle("a"));
    }

    #[test]
    fn test_three_node_cycle() {
        let items = vec![
            queued("a", &["c"]),
            queued("b", &["a"]),
            queued("c", &["b"]),
        ];
        let deps = FindingDeps::build(&items);
        assert!(deps.in_cycle("a"));
        assert!(deps.in_cycle("b"));
        assert!(deps.in_cycle("c"));
    }

    #[test]
    fn test_cycle_does_not_affect_non_cycle_items() {
        let items = vec![
            queued("a", &["b"]),
            queued("b", &["a"]),
            queued("c", &[]),
            queued("d", &["c"]),
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
        let items = vec![queued("a", &[])];
        let deps = FindingDeps::build(&items);
        let resolved = HashSet::new();
        assert!(deps.deps_met("a", &resolved));
    }

    #[test]
    fn test_deps_met_with_fixed_dep() {
        let items = vec![fixed("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_deps_met_with_wontfix_dep() {
        let items = vec![wontfix("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_deps_not_met_pending_dep() {
        let items = vec![pending("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(!deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_deps_not_met_queued_dep() {
        let items = vec![queued("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(!deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_deps_partially_met() {
        let items = vec![fixed("a", &[]), queued("b", &[]), queued("c", &["a", "b"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        // c depends on a (Fixed) and b (Queued) → not met
        assert!(!deps.deps_met("c", &resolved));
    }

    #[test]
    fn test_deps_all_met() {
        let items = vec![fixed("a", &[]), wontfix("b", &[]), queued("c", &["a", "b"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        // c depends on a (Fixed) and b (WontFix) → met
        assert!(deps.deps_met("c", &resolved));
    }

    #[test]
    fn test_deps_met_unknown_finding_id() {
        let items = vec![queued("a", &[])];
        let deps = FindingDeps::build(&items);
        let resolved = HashSet::new();
        // Unknown finding has no edges → treated as met
        assert!(deps.deps_met("nonexistent", &resolved));
    }

    // --- resolved_finding_ids ---

    #[test]
    fn test_resolved_ids_empty() {
        let items = vec![queued("a", &[]), pending("b", &[])];
        let resolved = resolved_finding_ids(&items);
        assert!(resolved.is_empty());
    }

    #[test]
    fn test_resolved_ids_fixed_and_wontfix() {
        let items = vec![
            fixed("a", &[]),
            wontfix("b", &[]),
            queued("c", &[]),
            pending("d", &[]),
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
        let items = vec![queued("a", &[]), queued("b", &["a"]), queued("c", &["b"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        assert!(deps.deps_met("a", &resolved));
        assert!(!deps.deps_met("b", &resolved));
        assert!(!deps.deps_met("c", &resolved));

        // After a is fixed
        let items2 = vec![fixed("a", &[]), queued("b", &["a"]), queued("c", &["b"])];
        let resolved2 = resolved_finding_ids(&items2);
        assert!(deps.deps_met("b", &resolved2));
        assert!(!deps.deps_met("c", &resolved2));

        // After b is also fixed
        let items3 = vec![fixed("a", &[]), fixed("b", &["a"]), queued("c", &["b"])];
        let resolved3 = resolved_finding_ids(&items3);
        assert!(deps.deps_met("c", &resolved3));
    }

    #[test]
    fn test_diamond_dependency() {
        // d depends on b and c, both depend on a
        let items = vec![
            queued("a", &[]),
            queued("b", &["a"]),
            queued("c", &["a"]),
            queued("d", &["b", "c"]),
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
            queued("b", &["a"]),
            queued("c", &["a"]),
            queued("d", &["b", "c"]),
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
            queued("d", &["b", "c"]),
        ];
        let resolved3 = resolved_finding_ids(&items3);
        assert!(deps.deps_met("d", &resolved3));
    }

    #[test]
    fn test_wontfix_unblocks_dependent() {
        let items = vec![wontfix("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(deps.deps_met("b", &resolved));
    }

    // --- unresolved_deps ---

    #[test]
    fn test_unresolved_deps_no_deps() {
        let items = vec![queued("a", &[])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(deps.unresolved_deps("a", &resolved).is_empty());
    }

    #[test]
    fn test_unresolved_deps_all_unresolved() {
        let items = vec![queued("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert_eq!(deps.unresolved_deps("b", &resolved), vec!["a"]);
    }

    #[test]
    fn test_unresolved_deps_partially_resolved() {
        let items = vec![fixed("a", &[]), queued("b", &[]), queued("c", &["a", "b"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert_eq!(deps.unresolved_deps("c", &resolved), vec!["b"]);
    }

    #[test]
    fn test_unresolved_deps_all_resolved() {
        let items = vec![fixed("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert!(deps.unresolved_deps("b", &resolved).is_empty());
    }

    #[test]
    fn test_unresolved_deps_sorted() {
        let items = vec![
            queued("z", &[]),
            queued("m", &[]),
            queued("a", &[]),
            queued("x", &["z", "m", "a"]),
        ];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);
        assert_eq!(deps.unresolved_deps("x", &resolved), vec!["a", "m", "z"]);
    }

    // --- Staleness detection ---

    #[test]
    fn test_is_stale_same_count() {
        let items = vec![queued("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(!deps.is_stale(2));
        assert_eq!(deps.item_count(), 2);
    }

    #[test]
    fn test_is_stale_different_count() {
        let items = vec![queued("a", &[]), queued("b", &["a"])];
        let deps = FindingDeps::build(&items);
        assert!(deps.is_stale(3));
        assert!(deps.is_stale(1));
    }

    #[test]
    fn test_cycle_members_skipped_non_cycle_proceed() {
        let items = vec![queued("a", &["b"]), queued("b", &["a"]), queued("c", &[])];
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        assert!(deps.in_cycle("a"));
        assert!(deps.in_cycle("b"));
        assert!(!deps.in_cycle("c"));
        assert!(deps.deps_met("c", &resolved));
    }
}
