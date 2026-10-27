//! Wiki-style links between contexts: `[[Title]]` or `[[Title|label]]`.
//!
//! Links are stored by target title (lowercased in SQL) and resolved at read
//! time, so they survive the target being created later and never point at a
//! context the reader cannot see.

use crate::validate::MAX_TITLE_CHARS;

/// Upper bound on distinct link targets stored per context.
pub const MAX_LINKS_PER_CONTEXT: usize = 200;

/// Distinct link targets in `content` (trimmed, compared case-insensitively),
/// in first-seen order. Links inside fenced code blocks and inline code spans
/// are ignored. Callers lowercase targets with SQL `lower()`, the same
/// function that normalizes titles when links are resolved.
pub fn wiki_links(content: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    let mut fence: Option<&str> = None;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(marker) = fence {
            if trimmed.starts_with(marker) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        for target in line_links(line) {
            if targets.len() == MAX_LINKS_PER_CONTEXT {
                return targets;
            }
            let key = target.to_lowercase();
            if !targets.iter().any(|t| t.to_lowercase() == key) {
                targets.push(target);
            }
        }
    }
    targets
}

fn line_links(line: &str) -> Vec<String> {
    // Drop inline code spans: text between matching backtick runs.
    let mut text = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        text.push_str(&rest[..start]);
        let run = rest[start..].chars().take_while(|&c| c == '`').count();
        let after = &rest[start + run..];
        let closing = "`".repeat(run);
        match after.find(&closing) {
            Some(end) => rest = &after[end + run..],
            None => {
                text.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    text.push_str(rest);

    let mut found = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        let target = inner.split('|').next().unwrap_or_default();
        let target = target.trim();
        if !target.is_empty()
            && !target.contains('[')
            && target.chars().count() <= MAX_TITLE_CHARS
            && !target.chars().any(char::is_control)
        {
            found.push(target.to_owned());
        }
        rest = &after[end + 2..];
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_normalized_distinct_links() {
        let md = "See [[API Auth]] and [[ api auth ]], plus [[Deploy|the deploy guide]].\n[[Deploy]]";
        assert_eq!(wiki_links(md), vec!["API Auth", "Deploy"]);
    }

    #[test]
    fn ignores_code_and_malformed_links() {
        let md = "`[[inline]]` text [[real]]\n```\n[[fenced]]\n```\n~~~md\n[[tilde]]\n~~~\n[[]] [[ ]] [[unclosed\n``[[double `tick`]]`` [[after]]";
        assert_eq!(wiki_links(md), vec!["real", "after"]);
    }

    #[test]
    fn caps_link_count_and_length() {
        let md: String = (0..300).map(|i| format!("[[page {i}]] ")).collect();
        assert_eq!(wiki_links(&md).len(), MAX_LINKS_PER_CONTEXT);
        let long = format!("[[{}]]", "x".repeat(MAX_TITLE_CHARS + 1));
        assert!(wiki_links(&long).is_empty());
    }
}
