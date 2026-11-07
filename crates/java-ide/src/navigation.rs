//! Navigation: definition, type definition, implementations, references, and
//! document highlights.
//!
//! References are found by a two-step process: a cheap textual filter picks
//! the files and tokens that could possibly match, then each candidate is
//! resolved and compared against the target declaration. The filter keeps it
//! fast; the resolution step is what makes the result correct.

use crate::convert;
use crate::target::{self, Target};
use java_semantic::infer::Inferer;
use java_semantic::name::{self, Name};
use java_semantic::scope::{self, Context};
use java_semantic::symbols::{ClassKind, FileId, Origin};
use java_semantic::{Db, Type};
use java_syntax::{ast, Node};
use lsp_types::{DocumentHighlight, DocumentHighlightKind, Location, LocationLink, Range};

/// Go to definition.
pub fn definition(db: &Db, file: FileId, offset: u32) -> Vec<LocationLink> {
    let Some(target) = target::target_at(db, file, offset) else {
        return Vec::new();
    };
    let Some(origin) = target.origin(db) else {
        return Vec::new();
    };
    let origin_range = convert::origin_link_ranges(db, &origin);
    let Some((location, selection)) = origin_range else {
        // A classpath declaration has no source to jump to.
        return Vec::new();
    };
    let origin_selection = target::identifier_at(db, file, offset)
        .and_then(|(range, _)| convert::range_of(db, file, range));
    vec![LocationLink {
        origin_selection_range: origin_selection,
        target_uri: location.uri,
        target_range: location.range,
        target_selection_range: selection,
    }]
}

/// Go to the definition of the *type* of the expression under the cursor.
pub fn type_definition(db: &Db, file: FileId, offset: u32) -> Vec<LocationLink> {
    let Some(context) = scope::context_at(db, file, offset) else {
        return Vec::new();
    };
    let inferer = Inferer::new(&context);
    let Some(node) = ast::identifier_at(context.source.tree(), offset) else {
        return Vec::new();
    };
    let ty = match target::target_of_node(&context, node) {
        Some(Target::Local { name, .. }) => context.local(&name).map(|l| l.ty.clone()),
        Some(Target::Field { owner, name }) => db
            .class(&owner)
            .and_then(|class| class.find_field(&name).map(|f| f.ty.clone())),
        Some(Target::Method { owner, index, .. }) => db
            .class(&owner)
            .and_then(|class| class.methods.get(index as usize).map(|m| m.ret.clone())),
        Some(Target::Class(binary)) => Some(Type::class(binary)),
        _ => ast::expression_ending_at(context.source.tree(), node.end_byte() as u32)
            .map(|expression| inferer.type_of(expression)),
    };
    let Some(ty) = ty else { return Vec::new() };
    // An array's type definition is its element type's.
    let mut base = ty;
    while let Type::Array(element) = base {
        base = *element;
    }
    let base = base.upper_bound();
    let Some(binary) = base.class_name() else {
        return Vec::new();
    };
    let Some(class) = db.class(binary) else {
        return Vec::new();
    };
    let Some((location, selection)) = convert::origin_link_ranges(db, &class.origin) else {
        return Vec::new();
    };
    vec![LocationLink {
        origin_selection_range: None,
        target_uri: location.uri,
        target_range: location.range,
        target_selection_range: selection,
    }]
}

/// Go to implementation: subtypes of a type, or overrides of a method.
pub fn implementations(db: &Db, file: FileId, offset: u32) -> Vec<Location> {
    let Some(target) = target::target_at(db, file, offset) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match target {
        Target::Class(binary) => {
            for (candidate, _) in db.workspace_class_names() {
                if candidate.as_ref() == binary.as_ref() {
                    continue;
                }
                let Some(class) = db.class(candidate) else {
                    continue;
                };
                let self_type = java_semantic::lower::self_type_of(&class);
                let is_subtype = java_semantic::members::supertypes(db, &self_type)
                    .iter()
                    .any(|s| s.class_name().map(|n| n == &binary).unwrap_or(false));
                if is_subtype {
                    if let Some(location) = convert::origin_location(db, &class.origin) {
                        out.push(location);
                    }
                }
            }
        }
        Target::Method { owner, index, .. } => {
            // An override has the same name and erased parameter types as the
            // declaration being asked about.
            let signature = match db.class(&owner).and_then(|class| {
                class
                    .methods
                    .get(index as usize)
                    .map(|m| m.erased_signature())
            }) {
                Some(signature) => signature,
                None => return Vec::new(),
            };
            for (candidate, _) in db.workspace_class_names() {
                let Some(class) = db.class(candidate) else {
                    continue;
                };
                if candidate.as_ref() == owner.as_ref() {
                    continue;
                }
                let self_type = java_semantic::lower::self_type_of(&class);
                let inherits = java_semantic::members::supertypes(db, &self_type)
                    .iter()
                    .any(|s| s.class_name().map(|n| n == &owner).unwrap_or(false));
                if !inherits {
                    continue;
                }
                let origin = class
                    .methods
                    .iter()
                    .find(|m| m.erased_signature() == signature)
                    .map(|m| m.origin.clone());
                if let Some(origin) = origin {
                    if let Some(location) = convert::origin_location(db, &origin) {
                        out.push(location);
                    }
                }
            }
        }
        _ => {}
    }
    out.sort_by(|a, b| {
        (a.uri.as_str(), a.range.start.line).cmp(&(b.uri.as_str(), b.range.start.line))
    });
    out.dedup_by(|a, b| a.uri == b.uri && a.range == b.range);
    out
}

/// Every reference to the declaration under the cursor.
pub fn references(db: &Db, file: FileId, offset: u32, include_declaration: bool) -> Vec<Location> {
    let Some(target) = target::target_at(db, file, offset) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (candidate_file, range) in reference_ranges(db, &target, include_declaration) {
        if let Some(location) = convert::location_of(db, candidate_file, range) {
            out.push(location);
        }
    }
    out
}

/// References within one file, as highlights.
pub fn document_highlights(db: &Db, file: FileId, offset: u32) -> Vec<DocumentHighlight> {
    let Some(target) = target::target_at(db, file, offset) else {
        return Vec::new();
    };
    let declaration = target.origin(db).and_then(|origin| origin.name_range());
    reference_ranges(db, &target, true)
        .into_iter()
        .filter(|(candidate, _)| *candidate == file)
        .filter_map(|(_, range)| {
            let kind = if Some(range) == declaration {
                DocumentHighlightKind::WRITE
            } else {
                DocumentHighlightKind::READ
            };
            Some(DocumentHighlight {
                range: convert::range_of(db, file, range)?,
                kind: Some(kind),
            })
        })
        .collect()
}

/// Byte ranges of every reference to `target`, across the workspace.
///
/// Returned as ranges rather than locations so rename can build text edits
/// from the same search.
pub fn reference_ranges(
    db: &Db,
    target: &Target,
    include_declaration: bool,
) -> Vec<(FileId, (u32, u32))> {
    let wanted = target.name().to_string();
    if wanted.is_empty() {
        return Vec::new();
    }
    let declaration_file = target.origin(db).and_then(|origin| origin.file());
    let declaration_range = target.origin(db).and_then(|origin| origin.name_range());

    let files: Vec<FileId> = if target.is_file_local() {
        declaration_file.into_iter().collect()
    } else {
        db.all_files().collect()
    };

    let mut out = Vec::new();
    for file in files {
        // Textual filter first: a file that does not contain the name cannot
        // contain a reference to it, and this skips most of the workspace.
        let Some(text) = db.text(file) else { continue };
        if !text.contains(&wanted) {
            continue;
        }
        let Some(source) = db.source(file) else {
            continue;
        };
        let Some(context) = scope::context_at(db, file, 0) else {
            continue;
        };
        let candidates = identifier_tokens(source.tree().root_node(), source.text(), &wanted);
        for node in candidates {
            let range = (node.start_byte() as u32, node.end_byte() as u32);
            if Some(range) == declaration_range && Some(file) == declaration_file {
                if include_declaration {
                    out.push((file, range));
                }
                continue;
            }
            // Resolution must happen in the context of this occurrence, not
            // the file's start, so that locals and shadowing are respected.
            let Some(local_context) = scope::context_at(db, file, range.0) else {
                continue;
            };
            if target::target_of_node(&local_context, node).as_ref() == Some(target) {
                out.push((file, range));
            }
        }
        let _ = context;
    }
    out.sort();
    out.dedup();
    out
}

/// Identifier tokens in a subtree with the given text.
fn identifier_tokens<'t>(root: Node<'t>, text: &str, wanted: &str) -> Vec<Node<'t>> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    let mut cursor = root.walk();
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier")
            && ast::text(node, text) == wanted
        {
            found.push(node);
        }
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    found
}

/// Classes in the workspace that directly extend or implement `binary`.
pub fn direct_subtypes(db: &Db, binary: &Name) -> Vec<Name> {
    let mut out = Vec::new();
    for (candidate, _) in db.workspace_class_names() {
        let Some(class) = db.class(candidate) else {
            continue;
        };
        let direct = class
            .superclass
            .iter()
            .chain(&class.interfaces)
            .any(|s| s.class_name().map(|n| n == binary).unwrap_or(false));
        if direct {
            out.push(candidate.clone());
        }
    }
    out.sort();
    out
}

/// The declaration a class's `extends`/`implements` clause points at, used by
/// code actions that need the supertype.
pub fn supertype_origins(db: &Db, binary: &str) -> Vec<(Name, Origin, ClassKind)> {
    let Some(class) = db.class(binary) else {
        return Vec::new();
    };
    class
        .superclass
        .iter()
        .chain(&class.interfaces)
        .filter_map(|supertype| {
            let name = supertype.class_name()?;
            let declared = db.class(name)?;
            Some((name.clone(), declared.origin.clone(), declared.kind))
        })
        .collect()
}

/// Range of the whole declaration a cursor is inside, for selection ranges.
pub fn enclosing_declaration_range(context: &Context<'_>, offset: u32) -> Option<Range> {
    let node = ast::node_at(context.source.tree(), offset);
    let declaration = ast::ancestor_of_kind(
        node,
        &[
            "method_declaration",
            "constructor_declaration",
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
        ],
    )?;
    convert::range_of(
        context.db,
        context.file,
        (
            declaration.start_byte() as u32,
            declaration.end_byte() as u32,
        ),
    )
}

/// The simple name of a target, for the rename prompt.
pub fn rename_placeholder(db: &Db, file: FileId, offset: u32) -> Option<(Range, String)> {
    let target = target::target_at(db, file, offset)?;
    // Only declarations the workspace owns can be renamed.
    let origin = target.origin(db)?;
    if !matches!(origin, Origin::Source { .. }) {
        return None;
    }
    let (range, text) = target::identifier_at(db, file, offset)?;
    let _ = name::simple_name(&text);
    Some((convert::range_of(db, file, range)?, text))
}
