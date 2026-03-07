//! Maps review findings to exact GitHub review comment targets from unified diffs.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock};

use regex::Regex;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DiffPositionMapperError {
    #[error("diff parse error: {0}")]
    Parse(String),
    #[error("invalid review line for {file}: {line}")]
    InvalidLine { file: String, line: u32 },
    #[error("file not found in diff: {0}")]
    FileNotFound(String),
    #[error("file has no current commentable path in diff: {0}")]
    NoCurrentPath(String),
}

static DIFF_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^diff --git "?a/(.+?)"? "?b/(.+?)"?$"#).expect("diff header regex is valid")
});

static HUNK_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").expect("hunk header regex is valid")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewCommentTarget {
    Line { path: String, line: u32 },
    File { path: String },
}

#[derive(Debug, Clone)]
pub struct DiffPositionMapper {
    files_by_alias: HashMap<String, DiffFile>,
}

#[derive(Debug, Clone)]
struct DiffFile {
    current_path: Option<Arc<String>>,
    commentable_lines: Arc<HashSet<u32>>,
}

#[derive(Debug, Clone, Copy)]
struct HunkCursor {
    old_line: u32,
    new_line: u32,
}

#[derive(Debug, Clone)]
struct PendingFile {
    old_path: String,
    new_path: String,
    rename_from: Option<String>,
    rename_to: Option<String>,
    deleted: bool,
    current_path: Option<String>,
    commentable_lines: HashSet<u32>,
}

impl PendingFile {
    fn new(old_path: String, new_path: String) -> Self {
        Self {
            old_path,
            new_path,
            rename_from: None,
            rename_to: None,
            deleted: false,
            current_path: None,
            commentable_lines: HashSet::new(),
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

    fn resolved_current_path(&self) -> Option<String> {
        if self.deleted {
            return None;
        }
        self.current_path.clone().or_else(|| {
            let primary = self.primary_path();
            if primary == "/dev/null" {
                None
            } else {
                Some(primary)
            }
        })
    }
}

fn normalize_path(path: &str) -> String {
    path.trim().trim_matches('"').to_string()
}

fn normalize_patch_path(path: &str) -> String {
    let path = normalize_path(path);
    if path == "/dev/null" {
        return path;
    }
    if let Some(stripped) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
        return stripped.to_string();
    }
    path
}

fn parse_u32(value: &str, context: &str) -> Result<u32, DiffPositionMapperError> {
    value.parse::<u32>().map_err(|e| {
        DiffPositionMapperError::Parse(format!("invalid {context} value '{value}': {e}"))
    })
}

fn finalize_pending_file(file: PendingFile, files_by_alias: &mut HashMap<String, DiffFile>) {
    let aliases = file.aliases();
    let resolved_current_path = file.resolved_current_path().map(Arc::new);
    let commentable_lines = Arc::new(file.commentable_lines);
    for alias in aliases {
        files_by_alias.insert(
            alias,
            DiffFile {
                current_path: resolved_current_path.clone(),
                commentable_lines: Arc::clone(&commentable_lines),
            },
        );
    }
}

impl DiffPositionMapper {
    pub fn from_diff(diff: &str) -> Result<Self, DiffPositionMapperError> {
        let mut files_by_alias: HashMap<String, DiffFile> = HashMap::new();
        let mut pending: Option<PendingFile> = None;
        let mut cursor: Option<HunkCursor> = None;

        for line in diff.lines() {
            if let Some(caps) = DIFF_HEADER_RE.captures(line) {
                if let Some(previous) = pending.take() {
                    finalize_pending_file(previous, &mut files_by_alias);
                }
                cursor = None;

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
                let rename_to = normalize_path(rename_to);
                current.current_path = Some(rename_to.clone());
                current.rename_to = Some(rename_to);
                continue;
            }
            if line == "deleted file mode" || line.starts_with("deleted file mode ") {
                current.deleted = true;
                current.current_path = None;
                continue;
            }
            if let Some(path) = line.strip_prefix("+++ ") {
                let path = normalize_patch_path(path);
                if path == "/dev/null" {
                    current.deleted = true;
                    current.current_path = None;
                } else {
                    current.deleted = false;
                    current.current_path = Some(path);
                }
                continue;
            }
            if let Some(caps) = HUNK_HEADER_RE.captures(line) {
                let old_line = parse_u32(
                    caps.get(1).expect("regex capture group exists").as_str(),
                    "hunk old start",
                )?;
                let new_line = parse_u32(
                    caps.get(3).expect("regex capture group exists").as_str(),
                    "hunk new start",
                )?;
                if let Some(count_match) = caps.get(2) {
                    parse_u32(count_match.as_str(), "hunk old length")?;
                }
                if let Some(count_match) = caps.get(4) {
                    parse_u32(count_match.as_str(), "hunk new length")?;
                }
                cursor = Some(HunkCursor { old_line, new_line });
                continue;
            }

            let Some(mut hunk_cursor) = cursor else {
                continue;
            };

            match line.as_bytes().first().copied() {
                Some(b'+') if !line.starts_with("+++") => {
                    current.commentable_lines.insert(hunk_cursor.new_line);
                    hunk_cursor.new_line =
                        hunk_cursor.new_line.checked_add(1).ok_or_else(|| {
                            DiffPositionMapperError::Parse("new line overflow".to_string())
                        })?;
                }
                Some(b' ') => {
                    current.commentable_lines.insert(hunk_cursor.new_line);
                    hunk_cursor.old_line =
                        hunk_cursor.old_line.checked_add(1).ok_or_else(|| {
                            DiffPositionMapperError::Parse("old line overflow".to_string())
                        })?;
                    hunk_cursor.new_line =
                        hunk_cursor.new_line.checked_add(1).ok_or_else(|| {
                            DiffPositionMapperError::Parse("new line overflow".to_string())
                        })?;
                }
                Some(b'-') if !line.starts_with("---") => {
                    hunk_cursor.old_line =
                        hunk_cursor.old_line.checked_add(1).ok_or_else(|| {
                            DiffPositionMapperError::Parse("old line overflow".to_string())
                        })?;
                }
                Some(b'\\') => {}
                _ => {}
            }
            cursor = Some(hunk_cursor);
        }

        if let Some(previous) = pending {
            finalize_pending_file(previous, &mut files_by_alias);
        }

        if !diff.trim().is_empty() && files_by_alias.is_empty() {
            return Err(DiffPositionMapperError::Parse(
                "diff did not contain any parseable file headers".to_string(),
            ));
        }

        Ok(Self { files_by_alias })
    }

    pub fn target_for(
        &self,
        file: &str,
        line: u32,
    ) -> Result<ReviewCommentTarget, DiffPositionMapperError> {
        let normalized_file = normalize_path(file);
        if line == 0 {
            return Err(DiffPositionMapperError::InvalidLine {
                file: normalized_file,
                line,
            });
        }

        let diff_file = self
            .files_by_alias
            .get(&normalized_file)
            .ok_or_else(|| DiffPositionMapperError::FileNotFound(normalized_file.clone()))?;

        let current_path = diff_file
            .current_path
            .as_ref()
            .map(|path| path.as_str())
            .ok_or_else(|| DiffPositionMapperError::NoCurrentPath(normalized_file.clone()))?
            .to_string();

        if diff_file.commentable_lines.contains(&line) {
            return Ok(ReviewCommentTarget::Line {
                path: current_path,
                line,
            });
        }

        Ok(ReviewCommentTarget::File { path: current_path })
    }

    pub fn current_path_for(&self, file: &str) -> Result<String, DiffPositionMapperError> {
        let normalized_file = normalize_path(file);
        let diff_file = self
            .files_by_alias
            .get(&normalized_file)
            .ok_or_else(|| DiffPositionMapperError::FileNotFound(normalized_file.clone()))?;

        diff_file
            .current_path
            .as_ref()
            .map(|path| path.to_string())
            .ok_or_else(|| DiffPositionMapperError::NoCurrentPath(normalized_file.clone()))
    }

    pub fn find_nearest_file(&self, file: &str) -> Option<String> {
        let path = Path::new(file);
        let mut dir = path.parent();
        let mut seen_paths = HashSet::<&str>::new();
        while let Some(current_dir) = dir {
            for df in self.files_by_alias.values() {
                let current_path = match df.current_path.as_ref() {
                    Some(current_path) => current_path,
                    None => continue,
                };
                if !seen_paths.insert(current_path.as_str()) {
                    continue;
                }
                if Path::new(current_path.as_str()).parent() == Some(current_dir)
                    && current_path.as_str() != file
                {
                    return Some(current_path.as_str().to_string());
                }
            }
            dir = current_dir.parent();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{DiffPositionMapper, DiffPositionMapperError, ReviewCommentTarget};

    const REALISTIC_UNIFIED_DIFF: &str =
        include_str!("../tests/fixtures/diff_position_mapper/realistic_unified.diff");

    #[test]
    fn test_target_for_returns_exact_line_when_request_is_commentable() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.target_for("src/foo.rs", 4).unwrap();

        assert_eq!(
            mapped,
            ReviewCommentTarget::Line {
                path: "src/foo.rs".to_string(),
                line: 4,
            }
        );
    }

    #[test]
    fn test_target_for_returns_error_for_file_missing_from_diff() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let err = mapper.target_for("src/missing.rs", 10).unwrap_err();

        assert_eq!(
            err,
            DiffPositionMapperError::FileNotFound("src/missing.rs".to_string())
        );
    }

    #[test]
    fn test_target_for_falls_back_to_file_comment_before_first_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.target_for("src/foo.rs", 1).unwrap();

        assert_eq!(
            mapped,
            ReviewCommentTarget::File {
                path: "src/foo.rs".to_string(),
            }
        );
    }

    #[test]
    fn test_target_for_falls_back_to_file_comment_between_hunks() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.target_for("src/foo.rs", 10).unwrap();

        assert_eq!(
            mapped,
            ReviewCommentTarget::File {
                path: "src/foo.rs".to_string(),
            }
        );
    }

    #[test]
    fn test_target_for_falls_back_to_file_comment_after_last_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let mapped = mapper.target_for("src/foo.rs", 99).unwrap();

        assert_eq!(
            mapped,
            ReviewCommentTarget::File {
                path: "src/foo.rs".to_string(),
            }
        );
    }

    #[test]
    fn test_target_for_handles_first_and_last_lines_of_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();

        let first = mapper.target_for("src/foo.rs", 3).unwrap();
        assert_eq!(
            first,
            ReviewCommentTarget::Line {
                path: "src/foo.rs".to_string(),
                line: 3,
            }
        );

        let last = mapper.target_for("src/foo.rs", 7).unwrap();
        assert_eq!(
            last,
            ReviewCommentTarget::Line {
                path: "src/foo.rs".to_string(),
                line: 7,
            }
        );
    }

    #[test]
    fn test_target_for_handles_single_line_hunk() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();

        let exact = mapper.target_for("src/single.rs", 42).unwrap();
        assert_eq!(
            exact,
            ReviewCommentTarget::Line {
                path: "src/single.rs".to_string(),
                line: 42,
            }
        );

        let fallback = mapper.target_for("src/single.rs", 44).unwrap();
        assert_eq!(
            fallback,
            ReviewCommentTarget::File {
                path: "src/single.rs".to_string(),
            }
        );
    }

    #[test]
    fn test_target_for_handles_renamed_files() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();

        let mapped_new_path = mapper.target_for("src/new_name.rs", 8).unwrap();
        assert_eq!(
            mapped_new_path,
            ReviewCommentTarget::Line {
                path: "src/new_name.rs".to_string(),
                line: 8,
            }
        );

        let mapped_old_path = mapper.target_for("src/old_name.rs", 7).unwrap();
        assert_eq!(
            mapped_old_path,
            ReviewCommentTarget::Line {
                path: "src/new_name.rs".to_string(),
                line: 7,
            }
        );
    }

    #[test]
    fn test_target_for_uses_file_comment_for_deletions_only_hunks_on_changed_file() {
        let diff = "\
diff --git a/src/deletions_only.rs b/src/deletions_only.rs
index aaaaaaa..bbbbbbb 100644
--- a/src/deletions_only.rs
+++ b/src/deletions_only.rs
@@ -10,2 +10,0 @@
-old1
-old2
";
        let mapper = DiffPositionMapper::from_diff(diff).unwrap();
        assert_eq!(
            mapper.target_for("src/deletions_only.rs", 10).unwrap(),
            ReviewCommentTarget::File {
                path: "src/deletions_only.rs".to_string(),
            }
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
        assert_eq!(
            mapper.target_for("src/file with spaces.rs", 2).unwrap(),
            ReviewCommentTarget::Line {
                path: "src/file with spaces.rs".to_string(),
                line: 2,
            }
        );
    }

    #[test]
    fn test_target_for_keeps_real_leading_a_path_component_distinct() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
@@ -1,0 +1,1 @@
+first
diff --git a/a/src/lib.rs b/a/src/lib.rs
@@ -10,0 +10,1 @@
+second
"#;
        let mapper = DiffPositionMapper::from_diff(diff).unwrap();

        assert_eq!(
            mapper.target_for("src/lib.rs", 1).unwrap(),
            ReviewCommentTarget::Line {
                path: "src/lib.rs".to_string(),
                line: 1,
            }
        );

        assert_eq!(
            mapper.target_for("a/src/lib.rs", 10).unwrap(),
            ReviewCommentTarget::Line {
                path: "a/src/lib.rs".to_string(),
                line: 10,
            }
        );
    }

    #[test]
    fn test_target_for_keeps_real_leading_b_path_component_distinct() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
@@ -1,0 +1,1 @@
+first
diff --git a/b/src/main.rs b/b/src/main.rs
@@ -20,0 +20,1 @@
+second
"#;
        let mapper = DiffPositionMapper::from_diff(diff).unwrap();

        assert_eq!(
            mapper.target_for("src/main.rs", 1).unwrap(),
            ReviewCommentTarget::Line {
                path: "src/main.rs".to_string(),
                line: 1,
            }
        );

        assert_eq!(
            mapper.target_for("b/src/main.rs", 20).unwrap(),
            ReviewCommentTarget::Line {
                path: "b/src/main.rs".to_string(),
                line: 20,
            }
        );
    }

    #[test]
    fn test_target_for_rejects_invalid_zero_line() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        assert_eq!(
            mapper.target_for("src/foo.rs", 0).unwrap_err(),
            DiffPositionMapperError::InvalidLine {
                file: "src/foo.rs".to_string(),
                line: 0,
            }
        );
    }

    #[test]
    fn test_target_for_rejects_deleted_files_without_current_path() {
        let diff = "\
diff --git a/src/deleted.rs b/src/deleted.rs
deleted file mode 100644
index 1234567..0000000
--- a/src/deleted.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-old1
-old2
";
        let mapper = DiffPositionMapper::from_diff(diff).unwrap();
        assert_eq!(
            mapper.target_for("src/deleted.rs", 1).unwrap_err(),
            DiffPositionMapperError::NoCurrentPath("src/deleted.rs".to_string())
        );
    }

    #[test]
    fn test_find_nearest_file_same_directory() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        // src/foo.rs is in the diff; looking for src/missing.rs should find a file in src/
        let result = mapper.find_nearest_file("src/missing.rs");
        assert!(result.is_some());
        let found = result.unwrap();
        assert!(
            found.starts_with("src/"),
            "expected src/ prefix, got {found}"
        );
    }

    #[test]
    fn test_find_nearest_file_no_match() {
        let mapper = DiffPositionMapper::from_diff(REALISTIC_UNIFIED_DIFF).unwrap();
        let result = mapper.find_nearest_file("completely/different/path.rs");
        assert!(result.is_none());
    }
}
