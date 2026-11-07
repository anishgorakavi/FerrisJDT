//! Signature help: which overload is being called, and which parameter the
//! cursor is in.

use crate::render;
use java_semantic::infer::Inferer;
use java_semantic::members::FoundMethod;
use java_semantic::overload;
use java_semantic::scope;
use java_semantic::symbols::FileId;
use java_semantic::{Db, Type};
use java_syntax::{ast, Node};
use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};

pub fn signature_help(db: &Db, file: FileId, offset: u32) -> Option<SignatureHelp> {
    let context = scope::context_at(db, file, offset)?;
    let source = context.source.clone();
    let node = ast::node_at(source.tree(), offset);

    // The innermost argument list the cursor is inside.
    let list = ast::ancestors(node).find(|ancestor| {
        ancestor.kind() == "argument_list"
            && (ancestor.start_byte() as u32) < offset
            && offset <= ancestor.end_byte() as u32
    })?;
    let call = list.parent()?;
    let active = active_parameter(list, offset, context.text());

    let inferer = Inferer::new(&context);
    let candidates: Vec<FoundMethod> = match call.kind() {
        "method_invocation" => inferer.call_candidates(call).map(|(_, found)| found)?,
        "object_creation_expression" | "explicit_constructor_invocation" => {
            let created = match call.kind() {
                "object_creation_expression" => ast::field(call, "type")
                    .map(|ty| context.type_scope().lower_type(ty, context.text()))?,
                _ => {
                    let is_super =
                        ast::named_children(call).any(|c| ast::text(c, context.text()) == "super");
                    if is_super {
                        context
                            .class
                            .as_ref()
                            .and_then(|c| c.superclass.clone())
                            .unwrap_or(Type::Unknown)
                    } else {
                        context.self_type.clone()?
                    }
                }
            };
            inferer.constructors_of(&created)
        }
        _ => return None,
    };
    if candidates.is_empty() {
        return None;
    }

    // Argument types so far, used to put the best-matching overload first.
    let args: Vec<Type> = ast::named_children(list)
        .map(|argument| inferer.type_of(argument))
        .collect();
    let by_reference: Vec<&FoundMethod> = candidates.iter().collect();
    let best = overload::select(db, &by_reference, &args)
        .map(|found| (found.owner.name.clone(), found.method.erased_signature()));

    let mut ordered: Vec<&FoundMethod> = candidates
        .iter()
        .filter(|found| {
            // Overloads that cannot take this many arguments are noise.
            found.method.params.len() > active as usize || found.method.modifiers.is_varargs
        })
        .collect();
    if ordered.is_empty() {
        ordered = candidates.iter().collect();
    }
    ordered.sort_by_key(|found| found.method.params.len());

    let active_signature = best
        .as_ref()
        .and_then(|(owner, signature)| {
            ordered.iter().position(|found| {
                found.owner.name == *owner && found.method.erased_signature() == *signature
            })
        })
        .unwrap_or(0);

    let signatures = ordered
        .iter()
        .map(|found| signature_information(found, active))
        .collect();

    Some(SignatureHelp {
        signatures,
        active_signature: Some(active_signature as u32),
        active_parameter: Some(active),
    })
}

fn signature_information(found: &FoundMethod, active: u32) -> SignatureInformation {
    let method = &found.method;
    let label = render::method_declaration(method);
    // Parameter ranges are offsets into the label, which is how clients
    // highlight the active one without re-parsing the text.
    let mut parameters = Vec::new();
    if let Some(open) = label.find('(') {
        let mut cursor = open + 1;
        for (index, param) in method.params.iter().enumerate() {
            let mut rendered = param.ty.display_short();
            if method.modifiers.is_varargs && index + 1 == method.params.len() {
                if let Some(element) = rendered.strip_suffix("[]") {
                    rendered = format!("{element}...");
                }
            }
            let text = format!("{rendered} {}", param.name);
            let start = label[cursor..]
                .find(&text)
                .map(|at| cursor + at)
                .unwrap_or(cursor);
            let end = start + text.len();
            cursor = end;
            parameters.push(ParameterInformation {
                label: ParameterLabel::LabelOffsets([start as u32, end as u32]),
                documentation: param_documentation(method, &param.name),
            });
        }
    }
    SignatureInformation {
        label,
        documentation: method.doc.as_deref().map(|doc| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: render::documentation(doc),
            })
        }),
        parameters: Some(parameters),
        active_parameter: Some(active.min(method.params.len().saturating_sub(1) as u32)),
    }
}

/// The `@param` text for one parameter, when the javadoc has it.
fn param_documentation(method: &java_semantic::MethodData, name: &str) -> Option<Documentation> {
    let doc = method.doc.as_deref()?;
    let (_, tags) = java_semantic::javadoc::split_tags(doc);
    let text = tags.iter().find_map(|(tag, body)| {
        if tag != "param" {
            return None;
        }
        let mut parts = body.splitn(2, char::is_whitespace);
        (parts.next()? == name).then(|| parts.next().unwrap_or("").trim().to_string())
    })?;
    Some(Documentation::String(text))
}

/// Which argument the cursor is in, counting commas at the top nesting level.
fn active_parameter(list: Node<'_>, offset: u32, text: &str) -> u32 {
    let mut index = 0u32;
    let mut cursor = list.walk();
    for child in list.children(&mut cursor) {
        if child.end_byte() as u32 > offset {
            break;
        }
        if ast::text(child, text) == "," {
            index += 1;
        }
    }
    index
}
