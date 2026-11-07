//! Rendering declarations as the text shown in completion, hover, and
//! signature help.

use java_semantic::javadoc;
use java_semantic::name;
use java_semantic::symbols::{ClassData, ClassKind, FieldData, MethodData, Visibility};
use java_semantic::Type;

/// `(int index, String value)`, with a varargs last parameter as `T...`.
pub fn parameter_list(method: &MethodData) -> String {
    let params: Vec<String> = method
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| {
            let mut ty = param.ty.display_short();
            if method.modifiers.is_varargs && index + 1 == method.params.len() {
                if let Some(element) = ty.strip_suffix("[]") {
                    ty = format!("{element}...");
                }
            }
            format!("{ty} {}", param.name)
        })
        .collect();
    format!("({})", params.join(", "))
}

fn modifier_prefix(visibility: Visibility, parts: &[(&str, bool)]) -> String {
    let mut out = String::new();
    let keyword = visibility.as_keyword();
    if !keyword.is_empty() {
        out.push_str(keyword);
        out.push(' ');
    }
    for (word, present) in parts {
        if *present {
            out.push_str(word);
            out.push(' ');
        }
    }
    out
}

/// A method declaration as it would be written in source.
pub fn method_declaration(method: &MethodData) -> String {
    let mut out = modifier_prefix(
        method.visibility,
        &[
            ("static", method.modifiers.is_static),
            ("final", method.modifiers.is_final),
            ("abstract", method.modifiers.is_abstract),
            ("default", method.modifiers.is_default),
            ("synchronized", method.modifiers.is_synchronized),
            ("native", method.modifiers.is_native),
        ],
    );
    if !method.type_params.is_empty() {
        let params: Vec<String> = method
            .type_params
            .iter()
            .map(|p| render_type_param(&p.name, &p.bounds))
            .collect();
        out.push_str(&format!("<{}> ", params.join(", ")));
    }
    if !method.is_constructor {
        out.push_str(&method.ret.display_short());
        out.push(' ');
    }
    out.push_str(&method.name);
    out.push_str(&parameter_list(method));
    if !method.throws.is_empty() {
        let thrown: Vec<String> = method.throws.iter().map(Type::display_short).collect();
        out.push_str(&format!(" throws {}", thrown.join(", ")));
    }
    out
}

pub fn field_declaration(field: &FieldData) -> String {
    let mut out = modifier_prefix(
        field.visibility,
        &[
            ("static", field.modifiers.is_static),
            ("final", field.modifiers.is_final),
            ("transient", field.modifiers.is_transient),
            ("volatile", field.modifiers.is_volatile),
        ],
    );
    out.push_str(&field.ty.display_short());
    out.push(' ');
    out.push_str(&field.name);
    if let Some(value) = &field.constant_value {
        out.push_str(" = ");
        out.push_str(value);
    }
    out
}

pub fn class_declaration(class: &ClassData) -> String {
    let mut out = modifier_prefix(
        class.visibility,
        &[
            ("static", class.modifiers.is_static),
            ("final", class.modifiers.is_final),
            (
                "abstract",
                class.modifiers.is_abstract && class.kind == ClassKind::Class,
            ),
        ],
    );
    out.push_str(class.kind.keyword());
    out.push(' ');
    out.push_str(class.simple_name());
    if !class.type_params.is_empty() {
        let params: Vec<String> = class
            .type_params
            .iter()
            .map(|p| render_type_param(&p.name, &p.bounds))
            .collect();
        out.push_str(&format!("<{}>", params.join(", ")));
    }
    if let Some(superclass) = &class.superclass {
        // Extending Object is implicit and not worth the noise.
        if superclass.class_name().map(|n| &**n) != Some("java.lang.Object") {
            out.push_str(&format!(" extends {}", superclass.display_short()));
        }
    }
    if !class.interfaces.is_empty() {
        let names: Vec<String> = class.interfaces.iter().map(Type::display_short).collect();
        let keyword = if class.kind == ClassKind::Interface {
            "extends"
        } else {
            "implements"
        };
        out.push_str(&format!(" {keyword} {}", names.join(", ")));
    }
    out
}

fn render_type_param(name: &str, bounds: &[Type]) -> String {
    let meaningful: Vec<String> = bounds
        .iter()
        .filter(|b| b.class_name().map(|n| &**n) != Some("java.lang.Object"))
        .map(Type::display_short)
        .collect();
    if meaningful.is_empty() {
        name.to_string()
    } else {
        format!("{name} extends {}", meaningful.join(" & "))
    }
}

/// A fenced Java code block, which is how hover shows a declaration.
pub fn code_block(code: &str) -> String {
    format!("```java\n{code}\n```")
}

/// Renders javadoc as markdown: the description, then the block tags as a list.
pub fn documentation(doc: &str) -> String {
    let (description, tags) = javadoc::split_tags(doc);
    let mut out = String::new();
    if !description.is_empty() {
        out.push_str(&convert_inline_tags(&description));
    }
    let mut rendered_tags = Vec::new();
    for (tag, body) in &tags {
        let body = convert_inline_tags(body);
        match tag.as_str() {
            "param" => {
                let mut parts = body.splitn(2, char::is_whitespace);
                let name = parts.next().unwrap_or("");
                let text = parts.next().unwrap_or("").trim();
                rendered_tags.push(format!("- `{name}` \u{2014} {text}"));
            }
            "return" => rendered_tags.push(format!("- **returns** \u{2014} {body}")),
            "throws" | "exception" => {
                let mut parts = body.splitn(2, char::is_whitespace);
                let name = parts.next().unwrap_or("");
                let text = parts.next().unwrap_or("").trim();
                rendered_tags.push(format!("- **throws** `{name}` \u{2014} {text}"));
            }
            "deprecated" => rendered_tags.push(format!("- **deprecated** \u{2014} {body}")),
            "see" => rendered_tags.push(format!("- *see* {body}")),
            _ => {}
        }
    }
    if !rendered_tags.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&rendered_tags.join("\n"));
    }
    out
}

/// Rewrites the javadoc inline tags that matter into markdown; `{@code x}`
/// becomes `` `x` `` and `{@link Foo#bar}` becomes `` `Foo.bar` ``.
fn convert_inline_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{@") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let body = &after[..end];
        let mut parts = body.splitn(2, char::is_whitespace);
        let tag = parts.next().unwrap_or("");
        let content = parts.next().unwrap_or("").trim();
        match tag {
            "code" | "literal" => out.push_str(&format!("`{content}`")),
            "link" | "linkplain" | "value" => {
                let target = content.split_whitespace().next().unwrap_or(content);
                out.push_str(&format!("`{}`", target.replace('#', ".")));
            }
            _ => out.push_str(content),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The qualified name shown above a hovered declaration.
pub fn owner_path(binary: &str) -> String {
    name::source_name(binary)
}
