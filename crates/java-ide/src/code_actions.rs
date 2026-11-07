//! Code actions: quick fixes for the diagnostics this server reports, plus the
//! source actions a Java file routinely needs.

use crate::convert;
use crate::diagnostics::{self, codes};
use crate::imports;
use crate::render;
use java_semantic::members::LookupOptions;
use java_semantic::name;
use java_semantic::scope;
use java_semantic::symbols::{ClassKind, FileId, Visibility};
use java_semantic::{Db, Type};
use java_syntax::ast;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Diagnostic, Position, Range, TextEdit, Uri,
    WorkspaceEdit,
};
use rustc_hash::FxHashMap;

/// Cap on import suggestions for one unresolved name.
const MAX_IMPORT_CANDIDATES: usize = 8;

pub fn code_actions(
    db: &Db,
    file: FileId,
    range: Range,
    context_diagnostics: &[Diagnostic],
) -> Vec<CodeActionOrCommand> {
    let mut out = Vec::new();
    let Some(uri) = convert::uri_of(db, file) else {
        return out;
    };

    for diagnostic in context_diagnostics {
        match diagnostics::code_of(diagnostic) {
            Some(codes::UNKNOWN_TYPE) => {
                add_import_fixes(db, file, &uri, diagnostic, &mut out);
            }
            Some(codes::UNUSED_IMPORT) => {
                let edit = TextEdit {
                    range: whole_lines(diagnostic.range),
                    new_text: String::new(),
                };
                out.push(action(
                    "Remove unused import".to_string(),
                    CodeActionKind::QUICKFIX,
                    &uri,
                    vec![edit],
                    Some(diagnostic.clone()),
                ));
            }
            _ => {}
        }
    }

    if let Some(edit) = imports::organize_imports(db, file) {
        out.push(action(
            "Organize imports".to_string(),
            CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
            &uri,
            vec![edit],
            None,
        ));
    }

    add_generation_actions(db, file, range, &uri, &mut out);
    out
}

/// Offers an import for each class on the classpath with the missing name.
fn add_import_fixes(
    db: &Db,
    file: FileId,
    uri: &Uri,
    diagnostic: &Diagnostic,
    out: &mut Vec<CodeActionOrCommand>,
) {
    let Some(offset) = convert::offset_of(db, file, diagnostic.range.start) else {
        return;
    };
    let Some((_, written)) = crate::target::identifier_at(db, file, offset) else {
        return;
    };
    let simple = name::simple_name(&written);
    let mut candidates = db.classes_named(simple);
    // Prefer the common packages: a student wanting `List` means java.util.
    candidates.sort_by_key(|binary| {
        let package = name::package_of(binary);
        let rank = match package {
            "java.util" | "java.lang" | "java.io" => 0,
            p if p.starts_with("java.") => 1,
            p if p.starts_with("javax.") => 2,
            "" => 3,
            _ => 4,
        };
        (rank, binary.len(), binary.clone())
    });
    for binary in candidates.iter().take(MAX_IMPORT_CANDIDATES) {
        if name::is_internal(binary) {
            continue;
        }
        let Some(edit) = imports::import_edit(db, file, binary) else {
            continue;
        };
        out.push(action(
            format!("Import `{}`", name::source_name(binary)),
            CodeActionKind::QUICKFIX,
            uri,
            vec![edit],
            Some(diagnostic.clone()),
        ));
    }
}

/// Generation actions available at the cursor's class: constructors, getters
/// and setters, and overrides of unimplemented methods.
fn add_generation_actions(
    db: &Db,
    file: FileId,
    range: Range,
    uri: &Uri,
    out: &mut Vec<CodeActionOrCommand>,
) {
    let Some(offset) = convert::offset_of(db, file, range.start) else {
        return;
    };
    let Some(outline) = db.outline(file) else {
        return;
    };
    let Some(declared) = outline.class_at(offset) else {
        return;
    };
    let Some(class) = db.class(&declared.binary) else {
        return;
    };

    // Insert before the closing brace of the class body.
    let Some(body_end) = class_body_end(db, file, declared.range) else {
        return;
    };
    let indent = "    ";
    let insert_at = convert::range_of(db, file, (body_end, body_end));
    let Some(insert_at) = insert_at else { return };

    let instance_fields: Vec<_> = class
        .fields
        .iter()
        .filter(|field| !field.modifiers.is_static)
        .collect();

    if !instance_fields.is_empty() {
        let params: Vec<String> = instance_fields
            .iter()
            .map(|field| format!("{} {}", field.ty.display_short(), field.name))
            .collect();
        let assignments: Vec<String> = instance_fields
            .iter()
            .map(|field| format!("{indent}{indent}this.{0} = {0};", field.name))
            .collect();
        let text = format!(
            "\n{indent}public {}({}) {{\n{}\n{indent}}}\n",
            declared.simple,
            params.join(", "),
            assignments.join("\n")
        );
        if !class
            .methods
            .iter()
            .any(|m| m.is_constructor && m.params.len() == instance_fields.len())
        {
            out.push(action(
                format!(
                    "Generate constructor using {} field(s)",
                    instance_fields.len()
                ),
                CodeActionKind::new("source.generate.constructor"),
                uri,
                vec![TextEdit {
                    range: insert_at,
                    new_text: text,
                }],
                None,
            ));
        }

        let mut accessors = String::new();
        for field in &instance_fields {
            let capitalized = capitalize(&field.name);
            let ty = field.ty.display_short();
            let is_boolean = matches!(field.ty, Type::Primitive(java_semantic::Primitive::Boolean));
            let getter = if is_boolean {
                format!("is{capitalized}")
            } else {
                format!("get{capitalized}")
            };
            if class.methods_named(&getter).is_empty() {
                accessors.push_str(&format!(
                    "\n{indent}public {ty} {getter}() {{\n{indent}{indent}return {};\n{indent}}}\n",
                    field.name
                ));
            }
            let setter = format!("set{capitalized}");
            if class.methods_named(&setter).is_empty() && !field.modifiers.is_final {
                accessors.push_str(&format!(
                    "\n{indent}public void {setter}({ty} {0}) {{\n{indent}{indent}this.{0} = {0};\n{indent}}}\n",
                    field.name
                ));
            }
        }
        if !accessors.is_empty() {
            out.push(action(
                "Generate getters and setters".to_string(),
                CodeActionKind::new("source.generate.accessors"),
                uri,
                vec![TextEdit {
                    range: insert_at,
                    new_text: accessors,
                }],
                None,
            ));
        }
    }

    // Methods inherited but not implemented.
    let self_type = java_semantic::lower::self_type_of(&class);
    let members = java_semantic::members::members_of(
        db,
        &self_type,
        LookupOptions {
            include_instance: true,
            include_static: false,
            ..LookupOptions::default()
        },
    );
    let missing: Vec<_> = members
        .methods
        .iter()
        .filter(|found| {
            found.depth > 0
                && found.method.modifiers.is_abstract
                && found.method.visibility != Visibility::Private
        })
        .filter(|found| {
            // Not already implemented on this class.
            !class
                .methods
                .iter()
                .any(|own| own.erased_signature() == found.method.erased_signature())
        })
        .collect();
    if !missing.is_empty() && class.kind == ClassKind::Class {
        let mut text = String::new();
        for found in &missing {
            let method = &found.method;
            // A generated stub throws rather than returning a made-up value,
            // so forgetting to fill it in fails loudly instead of silently.
            let body = format!("{indent}{indent}throw new UnsupportedOperationException();");
            text.push_str(&format!(
                "\n{indent}@Override\n{indent}{} {{\n{body}\n{indent}}}\n",
                render::method_declaration(method)
                    .replace("abstract ", "")
                    .replace("default ", "")
            ));
        }
        out.push(action(
            format!("Implement {} inherited method(s)", missing.len()),
            CodeActionKind::new("source.generate.overrides"),
            uri,
            vec![TextEdit {
                range: insert_at,
                new_text: text,
            }],
            None,
        ));
    }
}

/// Byte offset of the closing brace of a class body.
fn class_body_end(db: &Db, file: FileId, class_range: (u32, u32)) -> Option<u32> {
    let source = db.source(file)?;
    let node = ast::node_at(source.tree(), class_range.0);
    let declaration = ast::ancestor_of_kind(
        node,
        &[
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
        ],
    )?;
    let body = ast::field(declaration, "body")?;
    // Place the insertion on the line of the closing brace.
    Some((body.end_byte() as u32).saturating_sub(1))
}

fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn whole_lines(range: Range) -> Range {
    Range {
        start: Position {
            line: range.start.line,
            character: 0,
        },
        end: Position {
            line: range.end.line + 1,
            character: 0,
        },
    }
}

// `Uri` is never mutated once built, so keying on it is safe; clippy
// flags it because its inner representation has interior mutability.
#[allow(clippy::mutable_key_type)]
fn action(
    title: String,
    kind: CodeActionKind,
    uri: &Uri,
    edits: Vec<TextEdit>,
    diagnostic: Option<Diagnostic>,
) -> CodeActionOrCommand {
    let mut changes: FxHashMap<Uri, Vec<TextEdit>> = FxHashMap::default();
    changes.insert(uri.clone(), edits);
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(kind),
        diagnostics: diagnostic.map(|d| vec![d]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes.into_iter().collect()),
            document_changes: None,
            change_annotations: None,
        }),
        command: None,
        is_preferred: None,
        disabled: None,
        data: None,
    })
}

/// Resolves a position into the enclosing context, used by tests.
pub fn context_at(db: &Db, file: FileId, offset: u32) -> Option<scope::Context<'_>> {
    scope::context_at(db, file, offset)
}
