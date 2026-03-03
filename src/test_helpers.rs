use crate::review_schema::{ReviewFinding, Severity};
use crate::submission::Reaction;

/// Create a `ReviewFinding` with sensible defaults for tests.
pub fn make_finding(id: &str) -> ReviewFinding {
    ReviewFinding {
        id: id.to_string(),
        file: "src/main.rs".to_string(),
        line: 42,
        severity: Severity::Warning,
        description: format!("{id} description"),
        category: Some("correctness".to_string()),
        depends_on: vec![],
    }
}

/// Create a `ReviewFinding` with `depends_on` set.
pub fn make_finding_with_deps(id: &str, deps: &[&str]) -> ReviewFinding {
    ReviewFinding {
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
        ..make_finding(id)
    }
}

/// Create a `Vec<Reaction>` from `(content, id)` pairs for tests.
pub fn make_reactions(specs: &[(&str, u64)]) -> Vec<Reaction> {
    specs
        .iter()
        .map(|(content, id)| Reaction {
            id: *id,
            content: content.to_string(),
        })
        .collect()
}
