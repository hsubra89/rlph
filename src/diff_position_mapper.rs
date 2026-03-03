use std::collections::{HashMap, HashSet};

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffPosition {
    pub file: String,
    pub line: u32,
    pub used_fallback: bool,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DiffPositionMapperError {
    #[error("diff parse error: {0}")]
    Parse(String),
    #[error("file not found in diff: {0}")]
    FileNotFound(String),
    #[error("file has no mappable lines in diff: {0}")]
    NoMappableLines(String),
}

#[derive(Debug, Clone)]
pub struct DiffPositionMapper {
    file_hunks: HashMap<String, Vec<Hunk>>,
}

#[derive(Debug, Clone, Copy)]
struct Hunk {
    start: u32,
    end: u32,
}

impl Hunk {
    fn contains(self, line: u32) -> bool {
        (self.start..=self.end).contains(&line)
    }

    fn closest_line(self, line: u32) -> u32 {
        if line < self.start {
            self.start
        } else if line > self.end {
            self.end
        } else {
            line
        }
    }
}

#[derive(Debug, Clone)]
struct PendingFile {
    old_path: String,
    new_path: String,
    rename_from: Option<String>,
    rename_to: Option<String>,
    hunks: Vec<Hunk>,
}

impl PendingFile {
    fn new(old_path: String, new_path: String) -> Self {
        Self {
            old_path,
            new_path,
            rename_from: None,
            rename_to: None,
            hunks: Vec::new(),
        }
    }

    fn aliases(&self) -> HashSet<String> {
        let mut aliases = HashSet::new();
        aliases.insert(self.primary_path());
        aliases.insert(self.new_path.clone());
        aliases.insert(self.old_path.clone());
        if let Some(rename_from) = &self.rename_from {
            aliases.insert(rename_from.clone());
        }
        if let Some(rename_to) = &self.rename_to {
            aliases.insert(rename_to.clone());
        }
        aliases.retain(|path| path != "/dev/null");
        aliases
    }

    fn primary_path(&self) -> String {
        self.rename_to
            .as_ref()
            .cloned()
            .unwrap_or_else(|| self.new_path.clone())
    }
}

fn normalize_path(path: &str) -> String {
    path.trim().trim_matches('"').to_string()
}

fn parse_u32(value: &str, context: &str) -> Result<u32, DiffPositionMapperError> {
    value.parse::<u32>().map_err(|e| {
        DiffPositionMapperError::Parse(format!("invalid {context} value '{value}': {e}"))
    })
}

fn finalize_pending_file(file: PendingFile, file_hunks: &mut HashMap<String, Vec<Hunk>>) {
    let aliases = file.aliases();
    let mut hunks = file.hunks;
    hunks.sort_by_key(|h| h.start);
    for alias in aliases {
        file_hunks.insert(alias, hunks.clone());
    }
}

impl DiffPositionMapper {
    pub fn from_diff(diff: &str) -> Result<Self, DiffPositionMapperError> {
        let diff_header_re =
            Regex::new(r#"^diff --git "?a/(.+?)"? "?b/(.+?)"?$"#).map_err(|e| {
                DiffPositionMapperError::Parse(format!("invalid diff header regex: {e}"))
            })?;
        let hunk_header_re =
            Regex::new(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@").map_err(|e| {
                DiffPositionMapperError::Parse(format!("invalid hunk header regex: {e}"))
            })?;

        let mut file_hunks: HashMap<String, Vec<Hunk>> = HashMap::new();
        let mut pending: Option<PendingFile> = None;

        for line in diff.lines() {
            if let Some(caps) = diff_header_re.captures(line) {
                if let Some(previous) = pending.take() {
                    finalize_pending_file(previous, &mut file_hunks);
                }

                let old_path =
                    normalize_path(caps.get(1).expect("regex capture group exists").as_str());
                let new_path =
                    normalize_path(caps.get(2).expect("regex capture group exists").as_str());
                pending = Some(PendingFile::new(old_path, new_path));
                continue;
            }

            let Some(current) = pending.as_mut() else {
                continue;
            };

            if let Some(rename_from) = line.strip_prefix("rename from ") {
                current.rename_from = Some(normalize_path(rename_from));
                continue;
            }
            if let Some(rename_to) = line.strip_prefix("rename to ") {
                current.rename_to = Some(normalize_path(rename_to));
                continue;
            }
            if let Some(caps) = hunk_header_re.captures(line) {
                let start = parse_u32(
                    caps.get(1).expect("regex capture group exists").as_str(),
                    "hunk start",
                )?;
                let count = if let Some(count_match) = caps.get(2) {
                    parse_u32(count_match.as_str(), "hunk length")?
                } else {
                    1
                };
                if count == 0 {
                    continue;
                }
                let end = start.checked_add(count - 1).ok_or_else(|| {
                    DiffPositionMapperError::Parse(format!(
                        "hunk end overflow for start={start}, count={count}"
                    ))
                })?;
                current.hunks.push(Hunk { start, end });
            }
        }

        if let Some(previous) = pending {
            finalize_pending_file(previous, &mut file_hunks);
        }

        if !diff.trim().is_empty() && file_hunks.is_empty() {
            return Err(DiffPositionMapperError::Parse(
                "diff did not contain any parseable file headers".to_string(),
            ));
        }

        Ok(Self { file_hunks })
    }

    pub fn map(&self, file: &str, line: u32) -> Result<DiffPosition, DiffPositionMapperError> {
        let normalized_file = normalize_path(file);
        let hunks = self
            .file_hunks
            .get(&normalized_file)
            .ok_or_else(|| DiffPositionMapperError::FileNotFound(normalized_file.clone()))?;

        if hunks.is_empty() {
            return Err(DiffPositionMapperError::NoMappableLines(normalized_file));
        }

        if hunks.iter().copied().any(|hunk| hunk.contains(line)) {
            return Ok(DiffPosition {
                file: normalized_file,
                line,
                used_fallback: false,
            });
        }

        let mut best_line: Option<u32> = None;
        let mut best_distance = u32::MAX;
        for hunk in hunks.iter().copied() {
            let candidate = hunk.closest_line(line);
            let distance = line.abs_diff(candidate);
            if distance < best_distance
                || (distance == best_distance && best_line.is_none_or(|curr| candidate < curr))
            {
                best_distance = distance;
                best_line = Some(candidate);
            }
        }

        let mapped_line = best_line
            .ok_or_else(|| DiffPositionMapperError::NoMappableLines(normalized_file.clone()))?;
        Ok(DiffPosition {
            file: normalized_file,
            line: mapped_line,
            used_fallback: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{DiffPositionMapper, DiffPositionMapperError};

    const REALISTIC_UNIFIED_DIFF: &str =
        include_str!("../tests/fixtures/diff_position_mapper/realistic_unified.diff");

    #[test]
    fn test_map_returns_exact_line_when_request_is_within_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.map("src/foo.rs", 4).unwrap();

        assert_eq!(mapped.file, "src/foo.rs");
        assert_eq!(mapped.line, 4);
        assert!(!mapped.used_fallback);
    }

    #[test]
    fn test_map_returns_error_for_file_missing_from_diff() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let err = mapper.map("src/missing.rs", 10).unwrap_err();

        assert_eq!(
            err,
            DiffPositionMapperError::FileNotFound("src/missing.rs".to_string())
        );
    }

    #[test]
    fn test_map_uses_nearest_line_fallback_before_first_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.map("src/foo.rs", 1).unwrap();

        assert_eq!(mapped.line, 3);
        assert!(mapped.used_fallback);
    }

    #[test]
    fn test_map_uses_nearest_line_fallback_between_hunks() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.map("src/foo.rs", 10).unwrap();

        assert_eq!(mapped.line, 7);
        assert!(mapped.used_fallback);
    }

    #[test]
    fn test_map_uses_nearest_line_fallback_after_last_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.map("src/foo.rs", 99).unwrap();

        assert_eq!(mapped.line, 23);
        assert!(mapped.used_fallback);
    }

    #[test]
    fn test_map_handles_first_and_last_lines_of_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();

        let first = mapper.map("src/foo.rs", 3).unwrap();
        assert_eq!(first.line, 3);
        assert!(!first.used_fallback);

        let last = mapper.map("src/foo.rs", 7).unwrap();
        assert_eq!(last.line, 7);
        assert!(!last.used_fallback);
    }

    #[test]
    fn test_map_handles_single_line_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();

        let exact = mapper.map("src/single.rs", 42).unwrap();
        assert_eq!(exact.line, 42);
        assert!(!exact.used_fallback);

        let fallback = mapper.map("src/single.rs", 44).unwrap();
        assert_eq!(fallback.line, 42);
        assert!(fallback.used_fallback);
    }

    #[test]
    fn test_map_handles_renamed_files() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();

        let mapped_new_path = mapper.map("src/new_name.rs", 8).unwrap();
        assert_eq!(mapped_new_path.file, "src/new_name.rs");
        assert_eq!(mapped_new_path.line, 8);
        assert!(!mapped_new_path.used_fallback);

        let mapped_old_path = mapper.map("src/old_name.rs", 7).unwrap();
        assert_eq!(mapped_old_path.file, "src/old_name.rs");
        assert_eq!(mapped_old_path.line, 7);
        assert!(!mapped_old_path.used_fallback);
    }

    #[test]
    fn test_map_reports_no_mappable_lines_for_deletions_only_hunks() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let err = mapper.map("src/deletions_only.rs", 10).unwrap_err();
        assert_eq!(
            err,
            DiffPositionMapperError::NoMappableLines("src/deletions_only.rs".to_string())
        );
    }

    #[test]
    fn test_from_diff_rejects_non_diff_input() {
        let err = DiffPositionMapper::from_diff("not a unified diff").unwrap_err();
        assert_eq!(
            err,
            DiffPositionMapperError::Parse(
                "diff did not contain any parseable file headers".to_string()
            )
        );
    }

    #[test]
    fn test_from_diff_accepts_quoted_diff_headers() {
        let diff = r#"diff --git "a/src/file with spaces.rs" "b/src/file with spaces.rs"
@@ -0,0 +1,2 @@
+line 1
+line 2
"#;
        let mapper = DiffPositionMapper::from_diff(diff).unwrap();
        let mapped = mapper.map("src/file with spaces.rs", 2).unwrap();
        assert_eq!(mapped.file, "src/file with spaces.rs");
        assert_eq!(mapped.line, 2);
        assert!(!mapped.used_fallback);
    }

    #[test]
    fn test_map_keeps_real_leading_a_path_component_distinct() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
@@ -1,0 +1,1 @@
+first
diff --git a/a/src/lib.rs b/a/src/lib.rs
@@ -10,0 +10,1 @@
+second
"#;
        let mapper = DiffPositionMapper::from_diff(diff).unwrap();

        let mapped_root = mapper.map("src/lib.rs", 1).unwrap();
        assert_eq!(mapped_root.file, "src/lib.rs");
        assert_eq!(mapped_root.line, 1);
        assert!(!mapped_root.used_fallback);

        let mapped_nested = mapper.map("a/src/lib.rs", 10).unwrap();
        assert_eq!(mapped_nested.file, "a/src/lib.rs");
        assert_eq!(mapped_nested.line, 10);
        assert!(!mapped_nested.used_fallback);
    }

    #[test]
    fn test_map_keeps_real_leading_b_path_component_distinct() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
@@ -1,0 +1,1 @@
+first
diff --git a/b/src/main.rs b/b/src/main.rs
@@ -20,0 +20,1 @@
+second
"#;
        let mapper = DiffPositionMapper::from_diff(diff).unwrap();

        let mapped_root = mapper.map("src/main.rs", 1).unwrap();
        assert_eq!(mapped_root.file, "src/main.rs");
        assert_eq!(mapped_root.line, 1);
        assert!(!mapped_root.used_fallback);

        let mapped_nested = mapper.map("b/src/main.rs", 20).unwrap();
        assert_eq!(mapped_nested.file, "b/src/main.rs");
        assert_eq!(mapped_nested.line, 20);
        assert!(!mapped_nested.used_fallback);
    }
}
