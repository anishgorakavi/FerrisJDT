//! An open document: its text, parse tree, and position mapping, kept in step.

use crate::line_index::{LineIndex, Position};
use std::cell::RefCell;
use tree_sitter::{InputEdit, Parser, Point, Tree};

thread_local! {
    /// One parser per thread, reused across documents. A `Parser` owns a sizable
    /// scratch allocation, so giving every open file its own would waste memory
    /// for no gain; it is not `Sync`, which makes a thread-local the natural fit.
    static PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        parser
            .set_language(&crate::language())
            .expect("the bundled tree-sitter-java grammar is ABI-compatible");
        parser
    });
}

/// A syntax error reported by the parser, as a byte range plus a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub start: u32,
    pub end: u32,
    pub message: String,
}

/// One open document: its text, its parse tree, and the mapping between them.
///
/// Edits are applied to all three together so they can never disagree, and the
/// previous tree is always handed back to the parser so only the regions that
/// actually changed are re-parsed.
pub struct SourceFile {
    text: String,
    tree: Tree,
    line_index: LineIndex,
    version: i32,
}

impl SourceFile {
    pub fn new(text: String, version: i32) -> SourceFile {
        let tree = parse(&text, None);
        let line_index = LineIndex::new(&text);
        SourceFile {
            text,
            tree,
            line_index,
            version,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn line_index(&self) -> &LineIndex {
        &self.line_index
    }

    pub fn version(&self) -> i32 {
        self.version
    }

    pub fn offset_at(&self, position: Position) -> u32 {
        self.line_index.offset(&self.text, position)
    }

    pub fn position_at(&self, offset: u32) -> Position {
        self.line_index.position(&self.text, offset)
    }

    /// Replaces the whole document, as a `didChange` without a range does.
    pub fn set_text(&mut self, text: String, version: i32) {
        self.text = text;
        self.tree = parse(&self.text, None);
        self.line_index = LineIndex::new(&self.text);
        self.version = version;
    }

    /// Applies one ranged `didChange` edit and re-parses incrementally.
    pub fn apply_change(&mut self, start: Position, end: Position, new_text: &str, version: i32) {
        let start_byte = self.offset_at(start) as usize;
        let old_end_byte = (self.offset_at(end) as usize).max(start_byte);
        let new_end_byte = start_byte + new_text.len();

        let start_point = self.point_at(start_byte as u32);
        let old_end_point = self.point_at(old_end_byte as u32);

        self.text.replace_range(start_byte..old_end_byte, new_text);
        self.line_index = LineIndex::new(&self.text);
        let new_end_point = self.point_at(new_end_byte as u32);

        self.tree.edit(&InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position: start_point,
            old_end_position: old_end_point,
            new_end_position: new_end_point,
        });
        self.tree = parse(&self.text, Some(&self.tree));
        self.version = version;
    }

    /// tree-sitter counts a point's column in bytes, not UTF-16 units, so this
    /// deliberately does not reuse `LineIndex::position`.
    fn point_at(&self, offset: u32) -> Point {
        let position = self.position_at(offset);
        let line_start = self.line_index.line_start(position.line).unwrap_or(0);
        Point {
            row: position.line as usize,
            column: offset.saturating_sub(line_start) as usize,
        }
    }

    /// Collects parser error and missing nodes.
    ///
    /// tree-sitter recovers from errors rather than stopping, so a half-typed
    /// file still yields a usable tree; these are the places it had to recover.
    pub fn syntax_errors(&self) -> Vec<SyntaxError> {
        let mut errors = Vec::new();
        if !self.tree.root_node().has_error() {
            return errors;
        }
        let mut cursor = self.tree.walk();
        let mut visit_children = true;
        loop {
            let node = cursor.node();
            if visit_children && (node.is_error() || node.is_missing()) {
                let message = if node.is_missing() {
                    format!("missing `{}`", node.kind())
                } else {
                    let found = node
                        .child(0)
                        .map(|c| c.kind().to_string())
                        .unwrap_or_else(|| "token".to_string());
                    format!("unexpected {found}")
                };
                errors.push(SyntaxError {
                    start: node.start_byte() as u32,
                    // A missing node is zero-width; widen it so the editor can
                    // actually render the diagnostic.
                    end: (node.end_byte() as u32).max(node.start_byte() as u32 + 1),
                    message,
                });
                // Nested recoveries inside an error node add noise without
                // adding information, so stop descending here.
                visit_children = false;
            }
            // Descend only into subtrees that contain an error at all.
            if visit_children
                && (node.has_error() || node.is_missing())
                && cursor.goto_first_child()
            {
                continue;
            }
            visit_children = true;
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return errors;
                }
            }
        }
    }
}

fn parse(text: &str, old: Option<&Tree>) -> Tree {
    PARSER.with(|parser| {
        parser
            .borrow_mut()
            .parse(text, old)
            // `parse` returns None only on a cancellation or timeout, neither of
            // which this call configures.
            .expect("tree-sitter returns a tree when no cancellation is set")
    })
}
