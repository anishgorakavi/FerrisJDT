//! Syntax layer: tree-sitter parsing, incremental edits, and position mapping.

pub mod ast;
pub mod line_index;
pub mod source;

pub use line_index::{LineIndex, Position};
pub use source::{SourceFile, SyntaxError};
pub use tree_sitter::{Node, Point, Range, Tree, TreeCursor};

/// The tree-sitter grammar for Java, shared by every parser instance.
pub fn language() -> tree_sitter::Language {
    tree_sitter_java::LANGUAGE.into()
}
