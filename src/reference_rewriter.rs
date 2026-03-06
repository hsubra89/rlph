use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

pub(crate) static ISSUE_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/(?:issues|pull)/([0-9]+)|https://linear\.app/[A-Za-z0-9_-]+/issue/([A-Za-z0-9-]+)",
    )
    .expect("issue URL regex compiles")
});

static LINEAR_NUMERIC_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]+-([0-9]+)$").expect("linear suffix regex compiles"));

/// Rewrite GitHub/Linear issue URLs to local markdown links when the issue exists locally.
pub fn rewrite_issue_urls(markdown: &str, local_ids: &HashSet<String>) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut in_fenced_code_block = false;

    for line in markdown.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fenced_code_block = !in_fenced_code_block;
            out.push_str(line);
            continue;
        }

        if in_fenced_code_block {
            out.push_str(line);
            continue;
        }

        out.push_str(&rewrite_line(line, local_ids));
    }

    out
}

fn rewrite_line(line: &str, local_ids: &HashSet<String>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;

    for captures in ISSUE_URL_RE.captures_iter(line) {
        let Some(full_match) = captures.get(0) else {
            continue;
        };
        out.push_str(&line[last..full_match.start()]);

        if is_markdown_link_destination(line, full_match.start()) {
            out.push_str(full_match.as_str());
            last = full_match.end();
            continue;
        }

        let issue_id = captures
            .get(1)
            .or_else(|| captures.get(2))
            .map(|m| m.as_str())
            .expect("captured issue id");

        if let Some(local_file_id) = local_file_id(issue_id, local_ids) {
            out.push_str(&format!("[#{issue_id}](./{local_file_id}.md)"));
        } else {
            out.push_str(full_match.as_str());
        }
        last = full_match.end();
    }

    out.push_str(&line[last..]);
    out
}

fn local_file_id<'a>(issue_id: &'a str, local_ids: &'a HashSet<String>) -> Option<&'a str> {
    if local_ids.contains(issue_id) {
        return Some(issue_id);
    }

    let numeric = LINEAR_NUMERIC_SUFFIX_RE
        .captures(issue_id)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str())?;
    local_ids.contains(numeric).then_some(numeric)
}

fn is_markdown_link_destination(line: &str, url_start: usize) -> bool {
    let mut i = url_start;
    let bytes = line.as_bytes();

    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'(' {
        return false;
    }

    let mut j = i - 1;
    while j > 0 && bytes[j - 1].is_ascii_whitespace() {
        j -= 1;
    }

    j > 0 && bytes[j - 1] == b']'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> HashSet<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn rewrites_github_issue_url_when_local_file_exists() {
        let input = "See https://github.com/org/repo/issues/45";
        let rewritten = rewrite_issue_urls(input, &ids(&["45"]));
        assert_eq!(rewritten, "See [#45](./45.md)");
    }

    #[test]
    fn rewrites_github_pull_url_when_local_file_exists() {
        let input = "See https://github.com/org/repo/pull/45";
        let rewritten = rewrite_issue_urls(input, &ids(&["45"]));
        assert_eq!(rewritten, "See [#45](./45.md)");
    }

    #[test]
    fn rewrites_linear_issue_url_when_local_file_exists() {
        let input = "See https://linear.app/acme/issue/ENG-42";
        let rewritten = rewrite_issue_urls(input, &ids(&["ENG-42"]));
        assert_eq!(rewritten, "See [#ENG-42](./ENG-42.md)");
    }

    #[test]
    fn rewrites_linear_issue_url_when_local_numeric_file_exists() {
        let input = "See https://linear.app/acme/issue/ENG-42";
        let rewritten = rewrite_issue_urls(input, &ids(&["42"]));
        assert_eq!(rewritten, "See [#ENG-42](./42.md)");
    }

    #[test]
    fn leaves_url_unchanged_when_local_file_missing() {
        let input = "See https://github.com/org/repo/issues/45";
        let rewritten = rewrite_issue_urls(input, &ids(&["12"]));
        assert_eq!(rewritten, input);
    }

    #[test]
    fn handles_multiple_references_on_same_line() {
        let input =
            "Refs: https://github.com/org/repo/issues/45 and https://github.com/org/repo/pull/99";
        let rewritten = rewrite_issue_urls(input, &ids(&["45", "99"]));
        assert_eq!(rewritten, "Refs: [#45](./45.md) and [#99](./99.md)");
    }

    #[test]
    fn does_not_rewrite_urls_already_in_markdown_link_syntax() {
        let input = "See [issue](https://github.com/org/repo/issues/45)";
        let rewritten = rewrite_issue_urls(input, &ids(&["45"]));
        assert_eq!(rewritten, input);
    }

    #[test]
    fn rewrites_bare_urls_inside_markdown_text() {
        let input = "Context: https://github.com/org/repo/issues/45 for details.";
        let rewritten = rewrite_issue_urls(input, &ids(&["45"]));
        assert_eq!(rewritten, "Context: [#45](./45.md) for details.");
    }

    #[test]
    fn leaves_urls_inside_fenced_code_blocks_unchanged() {
        let input = "before\n```md\nhttps://github.com/org/repo/issues/45\n```\nafter\n";
        let rewritten = rewrite_issue_urls(input, &ids(&["45"]));
        assert_eq!(rewritten, input);
    }
}
