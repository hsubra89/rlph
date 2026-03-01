use crate::review_schema::{ReviewFinding, Severity};

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
