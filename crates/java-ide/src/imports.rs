//! Import handling: adding a missing import, and organizing the import block.

use java_semantic::name::{self, Name};
use java_semantic::outline::FileOutline;
use java_semantic::symbols::FileId;
use java_semantic::Db;
use lsp_types::TextEdit;
use rustc_hash::FxHashSet;

/// Whether a class can be referred to by its simple name already.
pub fn is_in_scope(outline: &FileOutline, binary: &str) -> bool {
    let package = name::package_of(binary);
    let simple = name::simple_name(binary);
    if package == outline.package.as_deref().unwrap_or("") || package == "java.lang" {
        return true;
    }
    // A nested class needs its outer class imported, or a qualified reference.
    if outline.classes.iter().any(|c| &*c.simple == simple) {
        return true;
    }
    outline.imports.iter().any(|import| {
        if import.is_static {
            return false;
        }
        if import.is_wildcard {
            &*import.path == package
        } else {
            &*import.path == binary || import.simple_name() == simple
        }
    })
}

/// A text edit that adds `binary` to the file's imports, or `None` when it is
/// already usable by simple name.
///
/// Imports are inserted in sorted position within the existing block, which
/// keeps the result stable no matter the order completions are accepted in.
pub fn import_edit(db: &Db, file: FileId, binary: &str) -> Option<TextEdit> {
    let outline = db.outline(file)?;
    if is_in_scope(&outline, binary) {
        return None;
    }
    // A nested class is imported by its binary name with dots.
    let statement = format!("import {};\n", name::source_name(binary));
    let source = db.source(file)?;
    let text = source.text();

    let non_static: Vec<_> = outline
        .imports
        .iter()
        .filter(|import| !import.is_static)
        .collect();
    let insert_at = match non_static
        .iter()
        .find(|import| import.path.as_ref() > binary)
    {
        // Before the first import that sorts after the new one.
        Some(following) => following.range.0,
        None => match non_static.last() {
            // After the last import, on the following line.
            Some(last) => line_after(text, last.range.1),
            None => after_package_declaration(db, file, text),
        },
    };
    let needs_blank_line = non_static.is_empty();
    let insertion = if needs_blank_line {
        format!("{statement}\n")
    } else {
        statement
    };
    let range = crate::convert::range_of(db, file, (insert_at, insert_at))?;
    Some(TextEdit {
        range,
        new_text: insertion,
    })
}

fn line_after(text: &str, offset: u32) -> u32 {
    match text[offset as usize..].find('\n') {
        Some(at) => offset + at as u32 + 1,
        None => text.len() as u32,
    }
}

fn after_package_declaration(db: &Db, file: FileId, text: &str) -> u32 {
    let Some(source) = db.source(file) else {
        return 0;
    };
    let root = source.tree().root_node();
    let package =
        java_syntax::ast::named_children(root).find(|n| n.kind() == "package_declaration");
    match package {
        Some(node) => {
            let after = line_after(text, node.end_byte() as u32);
            // Leave the blank line that conventionally follows the package.
            if text[after as usize..].starts_with('\n') {
                after + 1
            } else {
                after
            }
        }
        None => 0,
    }
}

/// The result of organizing imports: a single edit replacing the import block,
/// or `None` when nothing needs to change.
pub fn organize_imports(db: &Db, file: FileId) -> Option<TextEdit> {
    let outline = db.outline(file)?;
    if outline.imports.is_empty() {
        return None;
    }
    let source = db.source(file)?;
    let text = source.text();
    let used = used_simple_names(db, file);

    let mut kept: Vec<(bool, Name)> = Vec::new();
    let mut seen = FxHashSet::default();
    for import in &outline.imports {
        // A wildcard import cannot be checked for use, so it is always kept.
        let is_used = import.is_wildcard || used.contains(import.simple_name());
        if !is_used {
            continue;
        }
        if seen.insert((import.is_static, import.path.clone())) {
            kept.push((import.is_static, import.path.clone()));
        }
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut rendered = String::new();
    let mut last_static = None;
    for (is_static, path) in &kept {
        if last_static.is_some_and(|previous| previous != *is_static) {
            rendered.push('\n');
        }
        last_static = Some(*is_static);
        rendered.push_str("import ");
        if *is_static {
            rendered.push_str("static ");
        }
        rendered.push_str(path);
        let is_wildcard = outline
            .imports
            .iter()
            .any(|i| i.path == *path && i.is_static == *is_static && i.is_wildcard);
        if is_wildcard {
            rendered.push_str(".*");
        }
        rendered.push_str(";\n");
    }

    let start = outline.imports.first()?.range.0;
    let end = line_after(text, outline.imports.last()?.range.1);
    if text[start as usize..end as usize] == rendered {
        return None;
    }
    let range = crate::convert::range_of(db, file, (start, end))?;
    Some(TextEdit {
        range,
        new_text: rendered,
    })
}

/// Simple names mentioned anywhere in the file outside the import block.
///
/// This is a lexical check on identifier tokens, which is the right level for
/// deciding whether an import is referenced: an import is used if its name
/// appears, regardless of whether that occurrence resolves.
fn used_simple_names(db: &Db, file: FileId) -> FxHashSet<String> {
    let mut used = FxHashSet::default();
    let Some(source) = db.source(file) else {
        return used;
    };
    let Some(outline) = db.outline(file) else {
        return used;
    };
    let import_end = outline
        .imports
        .last()
        .map(|import| import.range.1)
        .unwrap_or(0);
    let text = source.text();
    let mut cursor = source.tree().walk();
    let mut stack = vec![source.tree().root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "import_declaration" {
            continue;
        }
        if matches!(node.kind(), "identifier" | "type_identifier")
            && node.start_byte() as u32 >= import_end
        {
            used.insert(java_syntax::ast::text(node, text).to_string());
        }
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    used
}
