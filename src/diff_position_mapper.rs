use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DiffPositionMapperError {
    #[error("diff parse error: {0}")]
    Parse(String),
    #[error("file not found in diff: {0}")]
    FileNotFound(String),
    #[error("file has no mappable lines in diff: {0}")]
    NoMappableLines(String),
}

static DIFF_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^diff --git "?a/(.+?)"? "?b/(.+?)"?$"#).expect("diff header regex is valid")
});

static HUNK_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@").expect("hunk header regex is valid")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackKind {
    None,
    Line,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffPosition {
    pub file: String,
    pub line: u32,
    pub fallback: FallbackKind,
}

#[derive(Debug, Clone)]
pub struct DiffPositionMapper {
    file_hunks: HashMap<String, Vec<Hunk>>,
    primary_paths: Vec<String>,
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
            .clone()
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

fn finalize_pending_file(
    file: PendingFile,
    file_hunks: &mut HashMap<String, Vec<Hunk>>,
) -> (String, bool) {
    let primary = file.primary_path();
    let aliases = file.aliases();
    let mut hunks = file.hunks;
    hunks.sort_by_key(|h| h.start);
    let has_mappable_hunks = !hunks.is_empty();
    for alias in aliases {
        file_hunks.insert(alias, hunks.clone());
    }
    (primary, has_mappable_hunks)
}

impl DiffPositionMapper {
    pub fn from_diff(diff: &str) -> Result<Self, DiffPositionMapperError> {
        let mut file_hunks: HashMap<String, Vec<Hunk>> = HashMap::new();
        let mut primary_paths: Vec<String> = Vec::new();
        let mut pending: Option<PendingFile> = None;

        for line in diff.lines() {
            if let Some(caps) = DIFF_HEADER_RE.captures(line) {
                if let Some(previous) = pending.take() {
                    let (primary, has_hunks) = finalize_pending_file(previous, &mut file_hunks);
                    if has_hunks {
                        primary_paths.push(primary);
                    }
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
            if let Some(caps) = HUNK_HEADER_RE.captures(line) {
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
            let (primary, has_hunks) = finalize_pending_file(previous, &mut file_hunks);
            if has_hunks {
                primary_paths.push(primary);
            }
        }

        if !diff.trim().is_empty() && file_hunks.is_empty() {
            return Err(DiffPositionMapperError::Parse(
                "diff did not contain any parseable file headers".to_string(),
            ));
        }

        primary_paths.sort();
        Ok(Self {
            file_hunks,
            primary_paths,
        })
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
                fallback: FallbackKind::None,
            });
        }

        let mut best_line = hunks[0].closest_line(line);
        let mut best_distance = line.abs_diff(best_line);
        for hunk in hunks[1..].iter().copied() {
            let candidate = hunk.closest_line(line);
            let distance = line.abs_diff(candidate);
            if distance < best_distance || (distance == best_distance && candidate < best_line) {
                best_distance = distance;
                best_line = candidate;
            }
        }

        let mapped_line = best_line;
        Ok(DiffPosition {
            file: normalized_file,
            line: mapped_line,
            fallback: FallbackKind::Line,
        })
    }

    pub fn map_with_file_fallback(
        &self,
        file: &str,
        line: u32,
    ) -> Result<DiffPosition, DiffPositionMapperError> {
        match self.map(file, line) {
            Ok(pos) => Ok(pos),
            Err(
                DiffPositionMapperError::FileNotFound(_)
                | DiffPositionMapperError::NoMappableLines(_),
            ) => {
                if let Some(nearby) = self.find_nearest_file(file) {
                    let hunks = self
                        .file_hunks
                        .get(&nearby)
                        .expect("nearest file exists in file_hunks");
                    let fallback_line = hunks[0].start;
                    Ok(DiffPosition {
                        file: nearby,
                        line: fallback_line,
                        fallback: FallbackKind::File,
                    })
                } else {
                    // Re-run to get the original error variant
                    self.map(file, line)
                }
            }
            Err(e) => Err(e),
        }
    }

    fn find_nearest_file(&self, file: &str) -> Option<String> {
        let path = Path::new(file);
        let mut dir = path.parent();
        while let Some(current_dir) = dir {
            let mut candidates: Vec<&str> = self
                .primary_paths
                .iter()
                .filter(|p| {
                    Path::new(p.as_str()).parent() == Some(current_dir) && p.as_str() != file
                })
                .map(|p| p.as_str())
                .collect();
            if !candidates.is_empty() {
                candidates.sort();
                return Some(candidates[0].to_string());
            }
            dir = current_dir.parent();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{DiffPositionMapper, DiffPositionMapperError, FallbackKind};

    const REALISTIC_UNIFIED_DIFF: &str =
        include_str!("../tests/fixtures/diff_position_mapper/realistic_unified.diff");

    #[test]
    fn test_map_returns_exact_line_when_request_is_within_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.map("src/foo.rs", 4).unwrap();

        assert_eq!(mapped.file, "src/foo.rs");
        assert_eq!(mapped.line, 4);
        assert_eq!(mapped.fallback, FallbackKind::None);
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
        assert_eq!(mapped.fallback, FallbackKind::Line);
    }

    #[test]
    fn test_map_uses_nearest_line_fallback_between_hunks() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.map("src/foo.rs", 10).unwrap();

        assert_eq!(mapped.line, 7);
        assert_eq!(mapped.fallback, FallbackKind::Line);
    }

    #[test]
    fn test_map_uses_nearest_line_fallback_after_last_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.map("src/foo.rs", 99).unwrap();

        assert_eq!(mapped.line, 23);
        assert_eq!(mapped.fallback, FallbackKind::Line);
    }

    #[test]
    fn test_map_handles_first_and_last_lines_of_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();

        let first = mapper.map("src/foo.rs", 3).unwrap();
        assert_eq!(first.line, 3);
        assert_eq!(first.fallback, FallbackKind::None);

        let last = mapper.map("src/foo.rs", 7).unwrap();
        assert_eq!(last.line, 7);
        assert_eq!(last.fallback, FallbackKind::None);
    }

    #[test]
    fn test_map_handles_single_line_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();

        let exact = mapper.map("src/single.rs", 42).unwrap();
        assert_eq!(exact.line, 42);
        assert_eq!(exact.fallback, FallbackKind::None);

        let fallback = mapper.map("src/single.rs", 44).unwrap();
        assert_eq!(fallback.line, 42);
        assert_eq!(fallback.fallback, FallbackKind::Line);
    }

    #[test]
    fn test_map_handles_renamed_files() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();

        let mapped_new_path = mapper.map("src/new_name.rs", 8).unwrap();
        assert_eq!(mapped_new_path.file, "src/new_name.rs");
        assert_eq!(mapped_new_path.line, 8);
        assert_eq!(mapped_new_path.fallback, FallbackKind::None);

        let mapped_old_path = mapper.map("src/old_name.rs", 7).unwrap();
        assert_eq!(mapped_old_path.file, "src/old_name.rs");
        assert_eq!(mapped_old_path.line, 7);
        assert_eq!(mapped_old_path.fallback, FallbackKind::None);
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
        assert_eq!(mapped.fallback, FallbackKind::None);
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
        assert_eq!(mapped_root.fallback, FallbackKind::None);

        let mapped_nested = mapper.map("a/src/lib.rs", 10).unwrap();
        assert_eq!(mapped_nested.file, "a/src/lib.rs");
        assert_eq!(mapped_nested.line, 10);
        assert_eq!(mapped_nested.fallback, FallbackKind::None);
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
        assert_eq!(mapped_root.fallback, FallbackKind::None);

        let mapped_nested = mapper.map("b/src/main.rs", 20).unwrap();
        assert_eq!(mapped_nested.file, "b/src/main.rs");
        assert_eq!(mapped_nested.line, 20);
        assert_eq!(mapped_nested.fallback, FallbackKind::None);
    }

    #[test]
    fn test_map_with_file_fallback_finds_sibling_file() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.map_with_file_fallback("src/missing.rs", 50).unwrap();

        assert_eq!(mapped.file, "src/foo.rs");
        assert_eq!(mapped.line, 3);
        assert_eq!(mapped.fallback, FallbackKind::File);
    }

    #[test]
    fn test_map_with_file_fallback_traverses_up() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper
            .map_with_file_fallback("src/deep/nested/missing.rs", 10)
            .unwrap();

        // No files in src/deep/nested/ or src/deep/, traverses up to src/ → picks src/foo.rs (first alphabetically)
        assert_eq!(mapped.file, "src/foo.rs");
        assert_eq!(mapped.line, 3);
        assert_eq!(mapped.fallback, FallbackKind::File);
    }

    #[test]
    fn test_map_with_file_fallback_errors_when_no_files_anywhere() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let err = mapper
            .map_with_file_fallback("completely/different/tree.rs", 1)
            .unwrap_err();

        assert_eq!(
            err,
            DiffPositionMapperError::FileNotFound("completely/different/tree.rs".to_string())
        );
    }

    #[test]
    fn test_map_with_file_fallback_skips_deletion_only_files() {
        let diff = "\
diff --git a/src/deletions_only.rs b/src/deletions_only.rs
@@ -10,2 +10,0 @@ fn gone() {
-old1
-old2
diff --git a/src/real.rs b/src/real.rs
@@ -1,0 +1,2 @@
+a
+b
";
        let mapper = DiffPositionMapper::from_diff(diff).unwrap();
        let mapped = mapper.map_with_file_fallback("src/missing.rs", 5).unwrap();

        // src/deletions_only.rs has no mappable hunks so it shouldn't be a fallback target
        assert_eq!(mapped.file, "src/real.rs");
        assert_eq!(mapped.line, 1);
        assert_eq!(mapped.fallback, FallbackKind::File);
    }
}
