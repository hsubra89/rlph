use std::collections::{HashMap, HashSet};

/// Result of Tarjan's strongly connected components algorithm.
pub struct TarjanScc<T> {
    pub components: Vec<Vec<T>>,
}

impl<T: Eq + std::hash::Hash + Clone + Ord> TarjanScc<T> {
    /// Compute all SCCs from a directed edge map.
    ///
    /// Nodes are collected from both keys and values of `edges`, sorted for
    /// deterministic output, then processed with Tarjan's algorithm.
    pub fn compute(edges: &HashMap<T, HashSet<T>>) -> Self {
        let mut all_nodes: HashSet<&T> = HashSet::new();
        for (src, dsts) in edges {
            all_nodes.insert(src);
            for dst in dsts {
                all_nodes.insert(dst);
            }
        }

        let mut nodes: Vec<&T> = all_nodes.into_iter().collect();
        nodes.sort();

        let mut state = TarjanState::default();
        for node in nodes {
            if !state.indices.contains_key(node) {
                strong_connect(node, edges, &mut state);
            }
        }

        Self {
            components: state.components,
        }
    }

    /// Return the set of nodes that belong to any non-trivial SCC (size > 1)
    /// or have a self-loop.
    pub fn cycle_members(&self, edges: &HashMap<T, HashSet<T>>) -> HashSet<T> {
        let mut members = HashSet::new();
        for component in &self.components {
            let has_self_loop = component
                .iter()
                .any(|node| edges.get(node).is_some_and(|deps| deps.contains(node)));
            if component.len() > 1 || has_self_loop {
                members.extend(component.iter().cloned());
            }
        }
        members
    }
}

struct TarjanState<T> {
    index: usize,
    indices: HashMap<T, usize>,
    lowlink: HashMap<T, usize>,
    stack: Vec<T>,
    on_stack: HashSet<T>,
    components: Vec<Vec<T>>,
}

impl<T> Default for TarjanState<T> {
    fn default() -> Self {
        Self {
            index: 0,
            indices: HashMap::new(),
            lowlink: HashMap::new(),
            stack: Vec::new(),
            on_stack: HashSet::new(),
            components: Vec::new(),
        }
    }
}

fn strong_connect<T: Eq + std::hash::Hash + Clone + Ord>(
    node: &T,
    edges: &HashMap<T, HashSet<T>>,
    state: &mut TarjanState<T>,
) {
    state.indices.insert(node.clone(), state.index);
    state.lowlink.insert(node.clone(), state.index);
    state.index += 1;
    state.stack.push(node.clone());
    state.on_stack.insert(node.clone());

    if let Some(deps) = edges.get(node) {
        let mut sorted_deps: Vec<&T> = deps.iter().collect();
        sorted_deps.sort();

        for dep in sorted_deps {
            if !state.indices.contains_key(dep) {
                strong_connect(dep, edges, state);
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
            let is_root = stack_node == *node;
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

    fn edges_from(pairs: &[(&str, &[&str])]) -> HashMap<String, HashSet<String>> {
        pairs
            .iter()
            .map(|(src, dsts)| {
                (
                    src.to_string(),
                    dsts.iter().map(|d| d.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn no_edges_no_cycles() {
        let edges: HashMap<String, HashSet<String>> = HashMap::new();
        let scc = TarjanScc::compute(&edges);
        assert!(scc.cycle_members(&edges).is_empty());
    }

    #[test]
    fn linear_chain_no_cycles() {
        let edges = edges_from(&[("a", &["b"]), ("b", &["c"])]);
        let scc = TarjanScc::compute(&edges);
        assert!(scc.cycle_members(&edges).is_empty());
    }

    #[test]
    fn two_node_cycle() {
        let edges = edges_from(&[("a", &["b"]), ("b", &["a"])]);
        let scc = TarjanScc::compute(&edges);
        let members = scc.cycle_members(&edges);
        assert_eq!(members.len(), 2);
        assert!(members.contains("a"));
        assert!(members.contains("b"));
    }

    #[test]
    fn three_node_cycle() {
        let edges = edges_from(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"])]);
        let scc = TarjanScc::compute(&edges);
        let members = scc.cycle_members(&edges);
        assert_eq!(members.len(), 3);
        assert!(members.contains("a"));
        assert!(members.contains("b"));
        assert!(members.contains("c"));
    }

    #[test]
    fn self_loop() {
        let edges = edges_from(&[("a", &["a"])]);
        let scc = TarjanScc::compute(&edges);
        let members = scc.cycle_members(&edges);
        assert_eq!(members.len(), 1);
        assert!(members.contains("a"));
    }

    #[test]
    fn mixed_cycle_and_non_cycle() {
        let edges = edges_from(&[
            ("a", &["b"]),
            ("b", &["a"]),
            ("c", &[]),
            ("d", &["c"]),
        ]);
        let scc = TarjanScc::compute(&edges);
        let members = scc.cycle_members(&edges);
        assert_eq!(members.len(), 2);
        assert!(members.contains("a"));
        assert!(members.contains("b"));
    }

    #[test]
    fn disjoint_cycles() {
        let edges = edges_from(&[
            ("a", &["b"]),
            ("b", &["a"]),
            ("x", &["y"]),
            ("y", &["x"]),
        ]);
        let scc = TarjanScc::compute(&edges);
        let members = scc.cycle_members(&edges);
        assert_eq!(members.len(), 4);
        assert!(members.contains("a"));
        assert!(members.contains("b"));
        assert!(members.contains("x"));
        assert!(members.contains("y"));
    }

    #[test]
    fn works_with_u64_keys() {
        let mut edges: HashMap<u64, HashSet<u64>> = HashMap::new();
        edges.insert(1, [2].into_iter().collect());
        edges.insert(2, [1].into_iter().collect());
        let scc = TarjanScc::compute(&edges);
        let members = scc.cycle_members(&edges);
        assert_eq!(members.len(), 2);
        assert!(members.contains(&1));
        assert!(members.contains(&2));
    }
}
