//! Document and workspace symbols.

use crate::convert;
use crate::render;
use java_semantic::name;
use java_semantic::outline::OutlineClass;
use java_semantic::symbols::{ClassKind, ClassRef, FileId};
use java_semantic::Db;
use java_syntax::ast;
use lsp_types::{
    DocumentSymbol, Location, SymbolInformation, SymbolKind, SymbolTag, WorkspaceSymbol,
};

/// Hierarchical symbols for one file: classes, then their members, nested
/// classes included.
pub fn document_symbols(db: &Db, file: FileId) -> Vec<DocumentSymbol> {
    let Some(outline) = db.outline(file) else {
        return Vec::new();
    };
    let classes = db.lowered_classes(file);
    let mut roots = Vec::new();
    for (index, declared) in outline.classes.iter().enumerate() {
        if declared.parent.is_some() {
            continue;
        }
        if let Some(symbol) = class_symbol(db, file, &outline, &classes, index as u32, declared) {
            roots.push(symbol);
        }
    }
    roots
}

fn class_symbol(
    db: &Db,
    file: FileId,
    outline: &java_semantic::outline::FileOutline,
    classes: &[ClassRef],
    index: u32,
    declared: &OutlineClass,
) -> Option<DocumentSymbol> {
    let range = convert::range_of(db, file, declared.range)?;
    let selection = convert::range_of(db, file, declared.name_range)?;
    let data = classes.iter().find(|c| c.name == declared.binary);

    let mut children = Vec::new();
    if let Some(data) = data {
        for field in &data.fields {
            let Some(origin_range) = field.origin.range() else {
                continue;
            };
            let Some(name_range) = field.origin.name_range() else {
                continue;
            };
            let Some(range) = convert::range_of(db, file, origin_range) else {
                continue;
            };
            let Some(selection) = convert::range_of(db, file, name_range) else {
                continue;
            };
            children.push(symbol(
                field.name.to_string(),
                Some(field.ty.display_short()),
                if field.modifiers.is_static && field.modifiers.is_final {
                    SymbolKind::CONSTANT
                } else {
                    SymbolKind::FIELD
                },
                field.modifiers.is_deprecated,
                range,
                selection,
                Vec::new(),
            ));
        }
        for method in &data.methods {
            let Some(origin_range) = method.origin.range() else {
                continue;
            };
            let Some(name_range) = method.origin.name_range() else {
                continue;
            };
            let Some(range) = convert::range_of(db, file, origin_range) else {
                continue;
            };
            let Some(selection) = convert::range_of(db, file, name_range) else {
                continue;
            };
            children.push(symbol(
                format!("{}{}", method.name, render::parameter_list(method)),
                Some(method.ret.display_short()),
                if method.is_constructor {
                    SymbolKind::CONSTRUCTOR
                } else {
                    SymbolKind::METHOD
                },
                method.modifiers.is_deprecated,
                range,
                selection,
                Vec::new(),
            ));
        }
    }
    // Nested classes, in declaration order.
    for (nested_index, nested) in outline.classes.iter().enumerate() {
        if nested.parent != Some(index) {
            continue;
        }
        if let Some(child) = class_symbol(db, file, outline, classes, nested_index as u32, nested) {
            children.push(child);
        }
    }
    children.sort_by_key(|child| (child.range.start.line, child.range.start.character));

    Some(symbol(
        declared.simple.to_string(),
        data.map(|d| render::class_declaration(d)),
        symbol_kind(declared.kind),
        data.map(|d| d.modifiers.is_deprecated).unwrap_or(false),
        range,
        selection,
        children,
    ))
}

#[allow(deprecated)] // DocumentSymbol::deprecated is deprecated but still part of the struct.
fn symbol(
    name: String,
    detail: Option<String>,
    kind: SymbolKind,
    deprecated: bool,
    range: lsp_types::Range,
    selection_range: lsp_types::Range,
    children: Vec<DocumentSymbol>,
) -> DocumentSymbol {
    DocumentSymbol {
        name,
        detail,
        kind,
        tags: deprecated.then(|| vec![SymbolTag::DEPRECATED]),
        deprecated: None,
        range,
        selection_range,
        children: (!children.is_empty()).then_some(children),
    }
}

fn symbol_kind(kind: ClassKind) -> SymbolKind {
    match kind {
        ClassKind::Class => SymbolKind::CLASS,
        ClassKind::Interface => SymbolKind::INTERFACE,
        ClassKind::Enum => SymbolKind::ENUM,
        ClassKind::Annotation => SymbolKind::INTERFACE,
    }
}

/// Cap on workspace symbol results, so a one-character query stays responsive.
const MAX_WORKSPACE_SYMBOLS: usize = 256;

/// Workspace symbols: classes and their members, matched by simple name.
pub fn workspace_symbols(db: &Db, query: &str) -> Vec<WorkspaceSymbol> {
    let mut out = Vec::new();
    let mut names: Vec<(java_semantic::Name, FileId)> = db
        .workspace_class_names()
        .map(|(name, file)| (name.clone(), file))
        .collect();
    names.sort();
    for (binary, file) in names {
        if out.len() >= MAX_WORKSPACE_SYMBOLS {
            break;
        }
        let simple = name::simple_name(&binary);
        let Some(class) = db.class(&binary) else {
            continue;
        };
        if crate::completion::matches_prefix(simple, query) || contains_ignore_case(simple, query) {
            if let Some(location) = convert::origin_location(db, &class.origin) {
                out.push(workspace_symbol(
                    simple.to_string(),
                    symbol_kind(class.kind),
                    Some(name::package_of(&binary).to_string()),
                    location,
                ));
            }
        }
        // Members are included so a student can jump straight to a method.
        for method in &class.methods {
            if out.len() >= MAX_WORKSPACE_SYMBOLS {
                break;
            }
            if !contains_ignore_case(&method.name, query) {
                continue;
            }
            if let Some(location) = convert::origin_location(db, &method.origin) {
                out.push(workspace_symbol(
                    format!("{}{}", method.name, render::parameter_list(method)),
                    if method.is_constructor {
                        SymbolKind::CONSTRUCTOR
                    } else {
                        SymbolKind::METHOD
                    },
                    Some(name::source_name(&binary)),
                    location,
                ));
            }
        }
        for field in &class.fields {
            if out.len() >= MAX_WORKSPACE_SYMBOLS {
                break;
            }
            if !contains_ignore_case(&field.name, query) {
                continue;
            }
            if let Some(location) = convert::origin_location(db, &field.origin) {
                out.push(workspace_symbol(
                    field.name.to_string(),
                    SymbolKind::FIELD,
                    Some(name::source_name(&binary)),
                    location,
                ));
            }
        }
        let _ = file;
    }
    out
}

fn workspace_symbol(
    name: String,
    kind: SymbolKind,
    container: Option<String>,
    location: Location,
) -> WorkspaceSymbol {
    WorkspaceSymbol {
        name,
        kind,
        tags: None,
        container_name: container,
        location: lsp_types::OneOf::Left(location),
        data: None,
    }
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Flat symbol list, for clients that ask for `SymbolInformation`.
#[allow(deprecated)] // SymbolInformation itself is deprecated in the protocol.
pub fn flat_document_symbols(db: &Db, file: FileId) -> Vec<SymbolInformation> {
    let Some(uri) = convert::uri_of(db, file) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack: Vec<(Option<String>, DocumentSymbol)> = document_symbols(db, file)
        .into_iter()
        .map(|symbol| (None, symbol))
        .collect();
    while let Some((container, symbol)) = stack.pop() {
        for child in symbol.children.clone().unwrap_or_default() {
            stack.push((Some(symbol.name.clone()), child));
        }
        out.push(SymbolInformation {
            name: symbol.name,
            kind: symbol.kind,
            tags: symbol.tags,
            deprecated: None,
            location: Location {
                uri: uri.clone(),
                range: symbol.range,
            },
            container_name: container,
        });
    }
    out
}

/// Selection ranges: the chain of enclosing syntax nodes at a position.
pub fn selection_ranges(db: &Db, file: FileId, offset: u32) -> Option<lsp_types::SelectionRange> {
    let source = db.source(file)?;
    let node = ast::node_at(source.tree(), offset);
    let mut chain: Vec<lsp_types::Range> = Vec::new();
    for ancestor in ast::ancestors(node) {
        if let Some(range) = convert::range_of(
            db,
            file,
            (ancestor.start_byte() as u32, ancestor.end_byte() as u32),
        ) {
            if chain.last() != Some(&range) {
                chain.push(range);
            }
        }
    }
    // Build from the outside in so each range's parent is the next one out.
    let mut current: Option<Box<lsp_types::SelectionRange>> = None;
    for range in chain.into_iter().rev() {
        current = Some(Box::new(lsp_types::SelectionRange {
            range,
            parent: current,
        }));
    }
    current.map(|boxed| *boxed)
}
