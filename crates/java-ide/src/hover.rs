//! Hover: what is this, and what does its documentation say.

use crate::convert;
use crate::render;
use crate::target::{self, Target};
use java_semantic::infer::Inferer;
use java_semantic::name;
use java_semantic::scope::{self, Resolved};
use java_semantic::symbols::{FileId, Origin};
use java_semantic::Db;
use java_syntax::{ast, Node};
use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

pub fn hover(db: &Db, file: FileId, offset: u32) -> Option<Hover> {
    let context = scope::context_at(db, file, offset)?;
    let node = ast::identifier_at(context.source.tree(), offset)?;
    let range = convert::range_of(db, file, (node.start_byte() as u32, node.end_byte() as u32));
    let target = target::target_of_node(&context, node);

    let mut sections: Vec<String> = Vec::new();
    match &target {
        Some(Target::Local { name, .. }) => {
            let local = context.local(name)?;
            let kind = if local.is_parameter {
                "parameter"
            } else {
                "local variable"
            };
            sections.push(render::code_block(&format!(
                "{} {}",
                local.ty.display_short(),
                local.name
            )));
            sections.push(format!("*{kind}*"));
        }
        Some(Target::Field { owner, name }) => {
            let class = db.class(owner)?;
            let field = class.find_field(name)?;
            sections.push(render::code_block(&render::field_declaration(field)));
            sections.push(owner_line(db, owner, &field.origin));
            if let Some(doc) = &field.doc {
                sections.push(render::documentation(doc));
            }
        }
        Some(Target::Method { owner, index, .. }) => {
            let class = db.class(owner)?;
            let method = class.methods.get(*index as usize)?;
            // At a call site the useful signature is the one with the
            // receiver's type arguments applied: `add(String e)` rather than
            // the declared `add(E e)`. The declaration is shown as well when
            // the two differ, since that is where the javadoc lives.
            let substituted = substituted_at(&context, node);
            let declared = render::method_declaration(method);
            match &substituted {
                Some(call) if *call != declared => {
                    sections.push(render::code_block(call));
                    sections.push(format!("*declared as* `{declared}`"));
                }
                _ => sections.push(render::code_block(&declared)),
            }
            sections.push(owner_line(db, owner, &method.origin));
            if let Some(doc) = &method.doc {
                sections.push(render::documentation(doc));
            }
        }
        Some(Target::Class(binary)) => {
            let class = db.class(binary)?;
            sections.push(render::code_block(&render::class_declaration(&class)));
            sections.push(owner_line(db, binary, &class.origin));
            if let Some(doc) = &class.doc {
                sections.push(render::documentation(doc));
            }
        }
        Some(Target::TypeParam { name, .. }) => {
            let bounds = context
                .type_vars
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, bounds)| bounds.clone())
                .unwrap_or_default();
            let rendered = match bounds.first() {
                Some(bound) => format!("{name} extends {}", bound.display_short()),
                None => name.to_string(),
            };
            sections.push(render::code_block(&rendered));
            sections.push("*type parameter*".to_string());
        }
        Some(Target::Package(package)) => {
            sections.push(render::code_block(&format!("package {package}")));
        }
        None => {
            // Not a declaration reference, but it may still be an expression
            // with a known type, which is worth reporting.
            let inferer = Inferer::new(&context);
            let expression =
                ast::expression_ending_at(context.source.tree(), node.end_byte() as u32)?;
            let ty = inferer.type_of(expression);
            if ty.is_unknown() {
                return None;
            }
            sections.push(render::code_block(&ty.display_short()));
        }
    }

    // The value's type is useful alongside a field or method declaration.
    if let Some(Target::Field { .. }) = &target {
        if let Some(Resolved::Field { field, .. }) =
            context.resolve_name(ast::text(node, context.text()))
        {
            let _ = field;
        }
    }

    let value = sections
        .into_iter()
        .filter(|section| !section.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    if value.is_empty() {
        return None;
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range,
    })
}

/// The signature of the method a call actually resolves to, with the
/// receiver's type arguments applied. `None` when the cursor is not on a call.
fn substituted_at(context: &scope::Context<'_>, node: Node<'_>) -> Option<String> {
    let parent = node.parent()?;
    if parent.kind() != "method_invocation" {
        return None;
    }
    if ast::field(parent, "name").map(|n| n.id()) != Some(node.id()) {
        return None;
    }
    let found = Inferer::new(context).resolve_invocation(parent)?;
    Some(render::method_declaration(&found.method))
}

/// A line naming where a declaration lives: its class, and for classpath
/// declarations the archive it was read from.
fn owner_line(db: &Db, owner: &str, origin: &Origin) -> String {
    let qualified = name::source_name(owner);
    match origin {
        Origin::Classpath { archive } => {
            let file = std::path::Path::new(&**archive)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| archive.to_string());
            format!("*{qualified}* \u{b7} `{file}`")
        }
        Origin::Source { file, .. } => {
            let path = db
                .path(*file)
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string());
            match path {
                Some(path) => format!("*{qualified}* \u{b7} `{path}`"),
                None => format!("*{qualified}*"),
            }
        }
        Origin::Synthetic => format!("*{qualified}* \u{b7} implicit"),
    }
}
