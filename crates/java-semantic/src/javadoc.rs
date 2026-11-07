//! Extraction of javadoc comments attached to a declaration.

use java_syntax::{ast, Node};
use std::sync::Arc;

/// The javadoc immediately preceding a declaration, with comment markers
/// stripped. Annotations and modifiers sit inside the declaration node, so the
/// comment is always a preceding sibling.
pub fn doc_for(node: Node<'_>, source: &str) -> Option<Arc<str>> {
    let mut previous = node.prev_sibling();
    while let Some(sibling) = previous {
        match sibling.kind() {
            "block_comment" => {
                let text = ast::text(sibling, source);
                return text
                    .starts_with("/**")
                    .then(|| Arc::from(strip_markers(text)));
            }
            // Keep scanning past the punctuation that can separate members.
            ";" | "," => previous = sibling.prev_sibling(),
            _ => return None,
        }
    }
    None
}

/// Removes `/**`, `*/`, and leading `*` from each line, preserving paragraph
/// structure and leaving javadoc tags in place for the hover renderer.
fn strip_markers(text: &str) -> String {
    let inner = text
        .strip_prefix("/**")
        .unwrap_or(text)
        .strip_suffix("*/")
        .unwrap_or(text);
    let mut lines: Vec<&str> = Vec::new();
    for line in inner.lines() {
        let trimmed = line.trim_start();
        let trimmed = trimmed.strip_prefix('*').unwrap_or(trimmed);
        lines.push(trimmed.strip_prefix(' ').unwrap_or(trimmed));
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Splits javadoc into its description and its block tags, so hover can render
/// `@param` and `@return` separately from the prose.
pub fn split_tags(doc: &str) -> (String, Vec<(String, String)>) {
    let mut description = Vec::new();
    let mut tags: Vec<(String, String)> = Vec::new();
    for line in doc.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('@') {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let tag = parts.next().unwrap_or("").to_string();
            let body = parts.next().unwrap_or("").trim().to_string();
            tags.push((tag, body));
        } else if let Some(last) = tags.last_mut() {
            // A tag's text may continue on the following lines.
            if !trimmed.is_empty() {
                last.1.push(' ');
                last.1.push_str(trimmed);
            }
        } else {
            description.push(line);
        }
    }
    (description.join("\n").trim().to_string(), tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comment_markers() {
        let text =
            "/**\n * Adds a value.\n *\n * @param value the value\n * @return true if added\n */";
        let stripped = strip_markers(text);
        assert_eq!(
            stripped,
            "Adds a value.\n\n@param value the value\n@return true if added"
        );
        let (description, tags) = split_tags(&stripped);
        assert_eq!(description, "Adds a value.");
        assert_eq!(tags.len(), 2);
        assert_eq!(
            tags[0],
            ("param".to_string(), "value the value".to_string())
        );
        assert_eq!(tags[1], ("return".to_string(), "true if added".to_string()));
    }

    #[test]
    fn joins_wrapped_tag_text() {
        let (_, tags) = split_tags("@param value a value\n  that wraps");
        assert_eq!(tags[0].1, "value a value that wraps");
    }
}
