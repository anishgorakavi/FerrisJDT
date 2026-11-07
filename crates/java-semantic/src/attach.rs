//! Recovering parameter names and documentation from a JDK's `src.zip`.
//!
//! Class files carry neither: parameter names survive only when a class was
//! compiled with `-parameters`, which the JDK is not, and javadoc is stripped
//! entirely. Without this, every standard library signature reads
//! `add(String arg0)` and hovering `ArrayList.add` says nothing at all.
//!
//! The source archive is right there in the JDK, and this server already has
//! a Java parser, so the names and the prose are taken from it. Only the
//! declarations are read — a source attachment never contributes types, which
//! stay as the class file describes them.

use crate::javadoc;
use crate::name;
use crate::symbols::ClassData;
use java_classfile::Jar;
use java_syntax::{ast, Node, SourceFile};
use std::sync::Arc;

/// What a source file says about one method that its class file cannot carry.
struct MethodSource {
    name: String,
    parameter_names: Vec<String>,
    doc: Option<Arc<str>>,
}

/// Documentation and parameter names for one class, keyed so they can be
/// matched onto declarations decoded from a class file.
#[derive(Default)]
pub struct Attachment {
    pub class_doc: Option<Arc<str>>,
    methods: Vec<MethodSource>,
    /// Field name, javadoc.
    fields: Vec<(String, Arc<str>)>,
}

impl Attachment {
    fn method(&self, name: &str, arity: usize) -> Option<&MethodSource> {
        // An exact arity match first; failing that, the only method with that
        // name, which covers a signature the class file widened.
        self.methods
            .iter()
            .find(|source| source.name == name && source.parameter_names.len() == arity)
            .or_else(|| {
                let mut matching = self.methods.iter().filter(|source| source.name == name);
                let first = matching.next()?;
                matching.next().is_none().then_some(first)
            })
    }

    fn field(&self, name: &str) -> Option<&Arc<str>> {
        self.fields
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, doc)| doc)
    }

    pub fn is_empty(&self) -> bool {
        self.class_doc.is_none() && self.methods.is_empty() && self.fields.is_empty()
    }
}

/// Archive entries that could hold the source of a class.
///
/// A nested class lives in its outer class's file, and JDK 9 and later put
/// every file under its module directory, which is not derivable from the
/// class name — so both spellings are offered and the first that exists wins.
pub fn source_entries(binary: &str) -> Vec<String> {
    let top_level = match binary.find('$') {
        Some(at) => &binary[..at],
        None => binary,
    };
    let path = top_level.replace('.', "/");
    let mut candidates = vec![format!("{path}.java")];
    // The common modules, in the order a standard library class is likely to
    // be in one. Probing is cheap: it is a binary search in the directory.
    for module in [
        "java.base",
        "java.desktop",
        "java.logging",
        "java.sql",
        "java.xml",
        "java.naming",
        "java.management",
        "java.rmi",
        "java.prefs",
        "java.net.http",
        "java.scripting",
        "java.compiler",
        "java.instrument",
        "java.datatransfer",
        "java.security.jgss",
        "java.transaction.xa",
    ] {
        candidates.push(format!("{module}/{path}.java"));
    }
    candidates
}

/// Reads a class's source from an archive and extracts what the class file
/// could not carry. Returns `None` when the source is absent or unreadable.
pub fn attachment_for(sources: &Jar, binary: &str) -> Option<Attachment> {
    let simple = name::simple_name(binary);
    let bytes = source_entries(binary)
        .into_iter()
        .find_map(|entry| sources.read(&entry).ok().flatten())?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let file = SourceFile::new(text, 0);
    let declaration = find_declaration(file.tree().root_node(), file.text(), simple)?;
    Some(extract(declaration, file.text()))
}

/// The declaration of `simple` anywhere in the file, including nested in
/// another class.
fn find_declaration<'t>(root: Node<'t>, text: &str, simple: &str) -> Option<Node<'t>> {
    let mut stack = vec![root];
    let mut cursor = root.walk();
    while let Some(node) = stack.pop() {
        if crate::outline::kind_of(node.kind()).is_some()
            && ast::name_text(node, text) == Some(simple)
        {
            return Some(node);
        }
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

fn extract(declaration: Node<'_>, text: &str) -> Attachment {
    let mut attachment = Attachment {
        class_doc: javadoc::doc_for(declaration, text),
        ..Attachment::default()
    };
    let Some(body) = ast::field(declaration, "body") else {
        return attachment;
    };
    let members: Vec<Node<'_>> = ast::named_children(body)
        .flat_map(|member| {
            if member.kind() == "enum_body_declarations" {
                ast::named_children(member).collect::<Vec<_>>()
            } else {
                vec![member]
            }
        })
        .collect();
    for member in members {
        match member.kind() {
            "method_declaration" | "constructor_declaration" => {
                let Some(name_node) = ast::field(member, "name") else {
                    continue;
                };
                attachment.methods.push(MethodSource {
                    name: ast::text(name_node, text).to_string(),
                    parameter_names: parameter_names(member, text),
                    doc: javadoc::doc_for(member, text),
                });
            }
            "field_declaration" | "constant_declaration" => {
                let Some(doc) = javadoc::doc_for(member, text) else {
                    continue;
                };
                for declarator in
                    ast::named_children(member).filter(|c| c.kind() == "variable_declarator")
                {
                    if let Some(name_node) = ast::field(declarator, "name") {
                        attachment
                            .fields
                            .push((ast::text(name_node, text).to_string(), doc.clone()));
                    }
                }
            }
            _ => {}
        }
    }
    attachment
}

fn parameter_names(method: Node<'_>, text: &str) -> Vec<String> {
    let Some(list) = ast::field(method, "parameters") else {
        return Vec::new();
    };
    ast::named_children(list)
        .filter_map(|parameter| match parameter.kind() {
            "formal_parameter" => Some(ast::text(ast::field(parameter, "name")?, text).to_string()),
            "spread_parameter" => {
                let declarator =
                    ast::named_children(parameter).find(|c| c.kind() == "variable_declarator")?;
                Some(ast::text(ast::field(declarator, "name")?, text).to_string())
            }
            _ => None,
        })
        .collect()
}

/// Copies an attachment's names and documentation onto a decoded class.
///
/// Only the fields a class file cannot carry are touched; every type comes
/// from the class file, which is authoritative.
pub fn apply(class: &mut ClassData, attachment: &Attachment) {
    if class.doc.is_none() {
        class.doc = attachment.class_doc.clone();
    }
    for method in &mut class.methods {
        let Some(source) = attachment.method(&method.name, method.params.len()) else {
            continue;
        };
        if method.doc.is_none() {
            method.doc = source.doc.clone();
        }
        // A constructor's class file signature may begin with a synthetic
        // parameter the source does not have, so names are only applied when
        // the counts agree.
        if source.parameter_names.len() == method.params.len() {
            for (param, declared) in method.params.iter_mut().zip(&source.parameter_names) {
                param.name = name::name(declared);
            }
        }
    }
    for field in &mut class.fields {
        if field.doc.is_none() {
            field.doc = attachment.field(&field.name).cloned();
        }
    }
}
