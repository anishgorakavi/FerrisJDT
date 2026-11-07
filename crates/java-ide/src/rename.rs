//! Rename across files.
//!
//! The edits come from the same semantic reference search that powers
//! `textDocument/references`, so a rename touches exactly the occurrences that
//! resolve to the renamed declaration.

use crate::convert;
use crate::navigation;
use crate::target;
use java_semantic::symbols::{FileId, Origin};
use java_semantic::Db;
use lsp_types::{TextEdit, Uri, WorkspaceEdit};
use rustc_hash::FxHashMap;

/// Why a rename was refused, so the client can say something useful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameError {
    NothingToRename,
    /// A classpath declaration: the source is not ours to edit.
    NotInWorkspace(String),
    InvalidIdentifier(String),
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenameError::NothingToRename => f.write_str("there is no symbol to rename here"),
            RenameError::NotInWorkspace(what) => {
                write!(
                    f,
                    "{what} is declared outside the workspace and cannot be renamed"
                )
            }
            RenameError::InvalidIdentifier(name) => {
                write!(f, "`{name}` is not a valid Java identifier")
            }
        }
    }
}

/// A Java identifier: a letter, `$`, or `_` followed by letters or digits, and
/// not a reserved word.
pub fn is_valid_identifier(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "abstract",
        "assert",
        "boolean",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "float",
        "for",
        "goto",
        "if",
        "implements",
        "import",
        "instanceof",
        "int",
        "interface",
        "long",
        "native",
        "new",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "static",
        "strictfp",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "try",
        "void",
        "volatile",
        "while",
        "true",
        "false",
        "null",
    ];
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    if !chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$') {
        return false;
    }
    !RESERVED.contains(&name)
}

// `Uri` is never mutated once built, so keying on it is safe; clippy
// flags it because its inner representation has interior mutability.
#[allow(clippy::mutable_key_type)]
pub fn rename(
    db: &Db,
    file: FileId,
    offset: u32,
    new_name: &str,
) -> Result<WorkspaceEdit, RenameError> {
    if !is_valid_identifier(new_name) {
        return Err(RenameError::InvalidIdentifier(new_name.to_string()));
    }
    let target = target::target_at(db, file, offset).ok_or(RenameError::NothingToRename)?;
    match target.origin(db) {
        Some(Origin::Source { .. }) => {}
        _ => return Err(RenameError::NotInWorkspace(target.name().to_string())),
    }

    let ranges = navigation::reference_ranges(db, &target, true);
    if ranges.is_empty() {
        return Err(RenameError::NothingToRename);
    }
    let mut changes: FxHashMap<Uri, Vec<TextEdit>> = FxHashMap::default();
    for (reference_file, range) in ranges {
        let Some(uri) = convert::uri_of(db, reference_file) else {
            continue;
        };
        let Some(range) = convert::range_of(db, reference_file, range) else {
            continue;
        };
        changes.entry(uri).or_default().push(TextEdit {
            range,
            new_text: new_name.to_string(),
        });
    }
    // Edits must not overlap and are easiest for clients to apply in order.
    for edits in changes.values_mut() {
        edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
        edits.dedup_by(|a, b| a.range == b.range);
    }
    Ok(WorkspaceEdit {
        changes: Some(changes.into_iter().collect()),
        document_changes: None,
        change_annotations: None,
    })
}
